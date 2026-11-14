mod config_io;
mod net;
mod registry;

use std::collections::HashMap;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::label::Label;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, Entity, SharedString, Window, WindowOptions, actions, div};

actions!(cargo_mirror_switcher, [Quit, ToggleTheme]);

/// Status line content + severity.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    Info(SharedString),
    Success(SharedString),
    Error(SharedString),
}

/// Latency measurement result for one registry.
#[derive(Debug, Clone)]
enum Latency {
    Measuring,
    Ok(Duration),
    Err(SharedString),
}

/// 列表行里展示的测速文本；未测过时返回 `None`（不渲染状态标签）。
fn latency_text(latency: Option<&Latency>) -> Option<String> {
    match latency {
        None => None,
        Some(Latency::Measuring) => Some("测速中…".to_string()),
        Some(Latency::Ok(d)) => Some(format!("{:.0} ms", d.as_secs_f64() * 1000.0)),
        Some(Latency::Err(e)) => Some(format!("失败：{}", truncate_detail(e, 24))),
    }
}

/// 按字符截断错误信息，超长时追加省略号。
fn truncate_detail(detail: &str, max_chars: usize) -> String {
    let mut chars = detail.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

/// Source of truth for the whole UI.
#[derive(Debug, Clone)]
struct State {
    selected: SharedString,
    current: Option<SharedString>,
    active_tab: usize,
    customs: HashMap<String, String>,
    status: Status,
    latencies: HashMap<String, Latency>,
    theme_dark: bool,
}

impl State {
    fn new(current: Option<String>, customs: HashMap<String, String>) -> Self {
        let selected = current.clone().unwrap_or_else(|| "rsproxy".to_string());
        Self {
            selected: selected.into(),
            current: current.map(Into::into),
            active_tab: 0,
            customs,
            status: Status::Info("选择一个源，然后点击「应用」写入 ~/.cargo/config.toml".into()),
            latencies: HashMap::new(),
            theme_dark: true,
        }
    }
}

/// Top-level application model.
struct CargoMirrorApp {
    state: State,
}

impl CargoMirrorApp {
    fn new(cx: &mut App) -> Entity<Self> {
        let current = config_io::current_source().ok().flatten();
        let customs = config_io::read_customs().unwrap_or_default();
        cx.new(|_cx| Self {
            state: State::new(current, customs),
        })
    }
}

fn main() {
    gpui_kit::application().run(|cx| {
        gpui_kit::init(cx);
        let app = CargoMirrorApp::new(cx);
        let _ = cx.open_window(
            WindowOptions {
                titlebar: Some(gpui_kit::TitlebarOptions {
                    title: Some("Cargo 镜像源切换工具".into()),
                    ..Default::default()
                }),
                window_bounds: Some(gpui_kit::WindowBounds::Windowed(gpui_kit::Bounds {
                    origin: gpui_kit::point(gpui_kit::px(200.), gpui_kit::px(200.)),
                    size: gpui_kit::size(gpui_kit::px(620.), gpui_kit::px(560.)),
                })),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| RootView::new(window, cx, app)),
        );
    });
}

/// The single root view, split into three panels.
struct RootView {
    app: Entity<CargoMirrorApp>,
    new_id_input: Entity<InputState>,
    new_url_input: Entity<InputState>,
}

impl RootView {
    fn new(window: &mut Window, cx: &mut Context<Self>, app: Entity<CargoMirrorApp>) -> Self {
        let new_id_input = cx.new(|cx| InputState::new(window, cx).placeholder("如 my-mirror"));
        let new_url_input = cx
            .new(|cx| InputState::new(window, cx).placeholder("sparse+https://example.com/index/"));
        Self {
            app,
            new_id_input,
            new_url_input,
        }
    }

    fn apply(&mut self, cx: &mut Context<Self>) {
        let selected: String = self.app.read(cx).state.selected.clone().into();
        let customs = self.app.read(cx).state.customs.clone();

        // Offload config write to a thread so we never block the UI.
        let selected_for_write = selected.clone();
        thread::spawn(move || {
            let result = (|| -> Result<_> {
                let content = registry::build_config(&selected_for_write, &customs);
                config_io::write_config(&content)
            })();
            result
        });

        // Update state synchronously for snappy feedback.
        self.app.update(cx, |model, cx| {
            model.state.current = Some(selected.clone().into());
            model.state.selected = selected.clone().into();
            model.state.status = Status::Success(
                format!("已切换到 {selected}，配置已写入（备份在 config.toml.bak）").into(),
            );
            cx.notify();
        });
    }

    fn measure(&mut self, id: String, url: String, cx: &mut Context<Self>) {
        self.app.update(cx, |model, cx| {
            model.state.latencies.insert(id.clone(), Latency::Measuring);
            cx.notify();
        });

        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { net::measure_latency(&url) })
                .await;
            let latency = match result {
                Ok(d) => Latency::Ok(d),
                Err(e) => Latency::Err(e.to_string().into()),
            };

            let _ = view.update(cx, |view, cx| {
                view.app.update(cx, |model, cx| {
                    model.state.latencies.insert(id, latency);
                    cx.notify();
                });
            });
        })
        .detach();
    }

    /// 单个源的测速状态展示：进行中 / 成功 / 失败。
    fn render_latency(&self, id: &str, cx: &mut Context<Self>) -> AnyElement {
        let latency = self.app.read(cx).state.latencies.get(id);
        let Some(text) = latency_text(latency) else {
            return div().into_any_element();
        };
        let color = match latency {
            Some(Latency::Measuring) => cx.theme().muted,
            Some(Latency::Ok(_)) => cx.theme().success,
            Some(Latency::Err(_)) => cx.theme().danger,
            None => cx.theme().muted,
        };

        Label::new(text)
            .text_color(color)
            .text_sm()
            .into_any_element()
    }

    fn render_preset_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let state = &self.app.read(cx).state;
        let selected = state.selected.clone();
        let current = state.current.clone();

        v_flex()
            .id("preset-list-scroll")
            .w_full()
            .flex_1()
            .min_h_0()
            .gap_2()
            .children(registry::builtins().into_iter().map(|r| {
                let id: SharedString = r.id.into();
                let is_selected = selected == id;
                let is_current = current.as_ref().map(|c| c.as_ref()) == Some(r.id);
                let status = if is_current { "当前生效" } else { "" };

                h_flex()
                    .id(format!("preset-{}", r.id))
                    .gap_3()
                    .items_center()
                    .p_3()
                    .rounded_lg()
                    .bg(if is_selected {
                        cx.theme().selection.alpha(0.25)
                    } else if is_current {
                        cx.theme().success.alpha(0.20)
                    } else {
                        cx.theme().transparent
                    })
                    .border_1()
                    .border_color(if is_selected {
                        cx.theme().selection
                    } else if is_current {
                        cx.theme().success
                    } else {
                        cx.theme().border
                    })
                    .when(is_selected, |f| f.cursor_pointer())
                    .child(
                        div()
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        Label::new(r.name)
                                            .text_lg()
                                            .font_semibold()
                                            .when(is_selected, |l| {
                                                l.text_color(cx.theme().selection)
                                            }),
                                    )
                                    .when(!status.is_empty(), |f| {
                                        f.child(
                                            Label::new(status)
                                                .text_color(cx.theme().success)
                                                .text_sm(),
                                        )
                                    }),
                            )
                            .child(
                                Label::new(r.description)
                                    .text_color(cx.theme().muted_foreground)
                                    .text_sm(),
                            ),
                    )
                    .child(
                        h_flex()
                            .ml_auto()
                            .items_center()
                            .gap_3()
                            .child(self.render_latency(r.id, cx))
                            .child(
                                Button::new(format!("test-{}", r.id))
                                    .label("测速")
                                    .ghost()
                                    .on_click(cx.listener(move |view, _ev, _window, cx| {
                                        let url = r.url.to_string();
                                        view.measure(r.id.to_string(), url, cx);
                                    })),
                            )
                            .child(
                                Button::new(format!("apply-{}", r.id))
                                    .label("应用")
                                    .when(is_selected, |b| b.primary().large())
                                    .when(!is_selected, |b| b.secondary())
                                    .on_click(cx.listener({
                                        let apply_id = id.clone();
                                        move |view, _ev, _window, cx| {
                                            view.app.update(cx, |model, cx| {
                                                model.state.selected = apply_id.clone();
                                                cx.notify();
                                            });
                                            view.apply(cx);
                                        }
                                    })),
                            ),
                    )
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |view, _ev, _window, cx| {
                            view.app.update(cx, |model, cx| {
                                model.state.selected = id.clone();
                                cx.notify();
                            });
                        }
                    }))
                    .into_any()
            }))
            .overflow_y_scrollbar()
            .into_any_element()
    }

    fn render_custom_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let state = &self.app.read(cx).state;
        let customs = state.customs.clone();
        let selected = state.selected.clone();
        let current = state.current.clone();

        if customs.is_empty() {
            return v_flex()
                .id("custom-list-scroll")
                .w_full()
                .flex_1()
                .min_h_0()
                .child(Label::new("还没有自定义源，在下方添加").text_color(cx.theme().muted_foreground))
                .overflow_y_scrollbar()
                .into_any_element();
        }

        let builtin_ids: Vec<&str> = registry::builtins().iter().map(|r| r.id).collect();
        v_flex()
            .id("custom-list-scroll")
            .w_full()
            .flex_1()
            .min_h_0()
            .gap_2()
            .children(
                customs
                    .into_iter()
                    .filter(|(id, _)| !builtin_ids.contains(&id.as_str()))
                    .map(|(id, url)| {
                let is_selected = selected.as_ref() == id;
                let is_current = current.as_ref().map(|c| c.as_ref()) == Some(id.as_str());
                let url_display = url.strip_prefix("sparse+").unwrap_or(&url).to_string();
                h_flex()
                    .id(format!("custom-{id}"))
                    .gap_3()
                    .items_center()
                    .p_3()
                    .rounded_lg()
                    .bg(if is_selected {
                        cx.theme().selection.alpha(0.25)
                    } else if is_current {
                        cx.theme().success.alpha(0.20)
                    } else {
                        cx.theme().transparent
                    })
                    .border_1()
                    .border_color(if is_selected {
                        cx.theme().selection
                    } else if is_current {
                        cx.theme().success
                    } else {
                        cx.theme().border
                    })
                    .child(
                        Button::new(format!("del-{id}"))
                            .label("删除")
                            .ghost()
                            .on_click(cx.listener({
                                let delete_id = id.clone();
                                move |view, _ev, _window, cx| {
                                    view.app.update(cx, |model, cx| {
                                        model.state.customs.remove(&delete_id);
                                        if model.state.selected.as_ref() == delete_id {
                                            model.state.selected = "rsproxy".into();
                                        }
                                        cx.notify();
                                    });
                                }
                            })),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                Label::new(id.clone())
                                    .font_semibold()
                                    .when(is_selected, |l| l.text_color(cx.theme().selection)),
                            )
                            .child(
                                Label::new(url_display)
                                    .text_color(cx.theme().muted_foreground)
                                    .text_sm(),
                            ),
                    )
                    .child(
                        h_flex()
                            .ml_auto()
                            .items_center()
                            .gap_3()
                            .child(self.render_latency(&id, cx))
                            .child(
                                Button::new(format!("test-custom-{id}"))
                                    .label("测速")
                                    .ghost()
                                    .on_click(cx.listener({
                                        let test_id = id.clone();
                                        let test_url = url.clone();
                                        move |view, _ev, _window, cx| {
                                            view.measure(test_id.clone(), test_url.clone(), cx);
                                        }
                                    })),
                            )
                            .child(
                                Button::new(format!("apply-custom-{id}"))
                                    .label("应用")
                                    .when(is_selected, |b| b.primary().large())
                                    .when(!is_selected, |b| b.secondary())
                                    .on_click(cx.listener({
                                        let apply_id = id.clone();
                                        move |view, _ev, _window, cx| {
                                            view.app.update(cx, |model, cx| {
                                                model.state.selected = apply_id.clone().into();
                                                cx.notify();
                                            });
                                            view.apply(cx);
                                        }
                                    })),
                            ),
                    )
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |view, _ev, _window, cx| {
                            view.app.update(cx, |model, cx| {
                                model.state.selected = id.clone().into();
                                cx.notify();
                            });
                        }
                    }))
                    .into_any()
            }))
            .overflow_y_scrollbar()
            .into_any_element()
    }

    fn render_add_form(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        v_flex()
                            .gap_1()
                            .flex_1()
                            .child(Label::new("标识名").text_sm())
                            .child(Input::new(&self.new_id_input).id("custom-id")),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .flex_grow(2.0)
                            .child(Label::new("Registry URL").text_sm())
                            .child(Input::new(&self.new_url_input).id("custom-url")),
                    )
                    .child(Button::new("add-custom").label("添加").primary().on_click(
                        cx.listener(|view, _ev, window, cx| {
                            let id: String = view.new_id_input.read(cx).value().trim().to_string();
                            let url: String =
                                view.new_url_input.read(cx).value().trim().to_string();
                            if id.is_empty() || url.is_empty() {
                                view.app.update(cx, |model, cx| {
                                    model.state.status =
                                        Status::Error("标识名和 URL 都不能为空".into());
                                    cx.notify();
                                });
                                return;
                            }
                            if registry::builtins().iter().any(|r| r.id == id) {
                                view.app.update(cx, |model, cx| {
                                    model.state.status =
                                        Status::Error(format!("「{id}」是内置源，不能覆盖").into());
                                    cx.notify();
                                });
                                return;
                            }
                            if view.app.read(cx).state.customs.contains_key(&id) {
                                view.app.update(cx, |model, cx| {
                                    model.state.status =
                                        Status::Error(format!("「{id}」已存在，请勿重复添加").into());
                                    cx.notify();
                                });
                                return;
                            }
                            view.app.update(cx, |model, cx| {
                                let url = if url.starts_with("sparse+") || url.starts_with("http") {
                                    url
                                } else {
                                    format!("sparse+https://{url}/")
                                };
                                model.state.customs.insert(id.clone(), url);
                                model.state.selected = id.into();
                                model.state.status = Status::Success("已添加自定义源".into());
                                cx.notify();
                            });
                            view.new_id_input
                                .update(cx, |state, cx| state.set_value("", window, cx));
                            view.new_url_input
                                .update(cx, |state, cx| state.set_value("", window, cx));
                        }),
                    )),
            )
            .into_any()
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let status = &self.app.read(cx).state.status;
        let (text, color) = match status {
            Status::Info(t) => (t.clone(), cx.theme().muted_foreground),
            Status::Success(t) => (t.clone(), cx.theme().success),
            Status::Error(t) => (t.clone(), cx.theme().danger),
        };
        h_flex()
            .p_3()
            .rounded_lg()
            .bg(cx.theme().muted)
            .border_1()
            .border_color(cx.theme().border)
            .w_full()
            .child(Label::new(text).text_color(color))
            .into_any()
    }
}

impl Render for RootView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.app.read(cx).state.theme_dark;
        let active_tab = self.app.read(cx).state.active_tab;
        let app = self.app.clone();

        v_flex()
            .size_full()
            .p_4()
            .gap_4()
            .bg(cx.theme().background)
            .child(
                // Header
                h_flex()
                    .items_center()
                    .child(Label::new("Cargo 镜像源切换工具").text_xl().font_bold())
                    .child(
                        div().ml_auto().child(
                            Button::new("toggle-theme")
                                .label(if theme { "浅色" } else { "深色" })
                                .ghost()
                                .on_click(cx.listener(|view, _ev, _window, cx| {
                                    view.app.update(cx, |model, cx| {
                                        model.state.theme_dark = !model.state.theme_dark;
                                        cx.notify();
                                    });
                                })),
                        ),
                    ),
            )
            // Main: independent preset and custom source tabs
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .gap_3()
                    .child(
                        TabBar::new("registry-tabs")
                            .w_full()
                            .segmented()
                            .selected_index(active_tab)
                            .on_click(move |index, _window, cx| {
                                app.update(cx, |model, cx| {
                                    model.state.active_tab = *index;
                                    cx.notify();
                                });
                            })
                            .child(Tab::new().flex_1().label("内置源"))
                            .child(Tab::new().flex_1().label("自定义源")),
                    )
                    .child(if active_tab == 0 {
                        self.render_preset_list(cx)
                    } else {
                        v_flex()
                            .flex_1()
                            .min_h_0()
                            .gap_3()
                            .child(self.render_custom_list(cx))
                            .child(self.render_add_form(cx))
                            .into_any_element()
                    }),
            )
            .child(self.render_status_bar(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latency_text_is_none_before_measuring() {
        assert_eq!(latency_text(None), None);
    }

    #[test]
    fn latency_text_shows_progress_while_measuring() {
        assert_eq!(
            latency_text(Some(&Latency::Measuring)).as_deref(),
            Some("测速中…")
        );
    }

    #[test]
    fn latency_text_formats_success_as_milliseconds() {
        let cases = [
            (Duration::from_millis(42), "42 ms"),
            (Duration::from_secs(1), "1000 ms"),
            (Duration::from_micros(400), "0 ms"),
        ];
        for (duration, expected) in cases {
            assert_eq!(
                latency_text(Some(&Latency::Ok(duration))).as_deref(),
                Some(expected)
            );
        }
    }

    #[test]
    fn latency_text_keeps_short_error_intact() {
        let latency = Latency::Err("HTTP 502".into());
        assert_eq!(
            latency_text(Some(&latency)).as_deref(),
            Some("失败：HTTP 502")
        );
    }

    #[test]
    fn latency_text_truncates_long_error() {
        let long: SharedString = "x".repeat(30).into();
        let latency = Latency::Err(long);
        let expected = format!("失败：{}…", "x".repeat(24));
        assert_eq!(latency_text(Some(&latency)), Some(expected));
    }

    #[test]
    fn latency_text_does_not_truncate_exact_limit() {
        let exact: SharedString = "y".repeat(24).into();
        let latency = Latency::Err(exact);
        let expected = format!("失败：{}", "y".repeat(24));
        assert_eq!(latency_text(Some(&latency)), Some(expected));
    }

    #[test]
    fn truncate_detail_counts_chars_not_bytes() {
        assert_eq!(truncate_detail("连接超时", 3), "连接超…");
        assert_eq!(truncate_detail("abc", 3), "abc");
    }
}
