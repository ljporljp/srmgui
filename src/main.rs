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
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, Entity, SharedString, Window, WindowOptions, actions, div, px};

actions!(cargo_mirror_switcher, [Quit, ToggleTheme]);

/// Status line content + severity.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    Info(SharedString),
    Success(SharedString),
    Error(SharedString),
}

/// 自动测速周期。
const PROBE_INTERVAL: Duration = Duration::from_secs(180);

/// 单层探测在 UI 中的状态。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Level {
    /// 正在探测。
    Running,
    /// 前序层失败，未执行。
    Skipped,
    /// 通过，附带详情。
    Passed(SharedString),
    /// 失败，附带错误信息。
    Failed(SharedString),
}

impl Level {
    fn from_layer(layer: net::Layer) -> Self {
        match layer {
            net::Layer::Skipped => Level::Skipped,
            net::Layer::Passed(detail) => Level::Passed(detail.into()),
            net::Layer::Failed(error) => Level::Failed(error.into()),
        }
    }
}

/// 一个源的四层探测状态：TCP → HTTP 索引探测 → 索引文件解析 → 下载速度。
#[derive(Debug, Clone)]
struct Probe {
    tcp: Level,
    http: Level,
    parse: Level,
    download: Level,
}

impl Probe {
    /// 测速进行中的占位状态。
    fn running() -> Self {
        Self {
            tcp: Level::Running,
            http: Level::Running,
            parse: Level::Running,
            download: Level::Running,
        }
    }

    fn from_report(report: net::ProbeReport) -> Self {
        Self {
            tcp: Level::from_layer(report.tcp),
            http: Level::from_layer(report.http),
            parse: Level::from_layer(report.parse),
            download: Level::from_layer(report.download),
        }
    }
}

/// 截断过长的文字（按字符），避免单行被撑开。
fn shorten(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

/// 单个步骤的展示文字与语义状态。
fn level_display(level: &Level) -> (String, LevelTone) {
    match level {
        Level::Running => ("⏳".to_string(), LevelTone::Pending),
        Level::Skipped => ("—".to_string(), LevelTone::Skipped),
        Level::Passed(detail) => (detail.to_string(), LevelTone::Passed),
        Level::Failed(error) => (shorten(error, 24), LevelTone::Failed),
    }
}

/// 步骤语义状态，用于决定文字颜色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LevelTone {
    Pending,
    Passed,
    Failed,
    Skipped,
}

/// 步骤悬停提示：说明该步骤的含义与当前状态。
fn level_tip(title: &str, level: &Level) -> String {
    match level {
        Level::Running => format!("{title}：测速中…"),
        Level::Skipped => format!("{title}：已跳过（前序步骤未通过）"),
        Level::Passed(detail) => format!("{title}：通过 · {detail}"),
        Level::Failed(error) => format!("{title}：不通过 · {error}"),
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
    probes: HashMap<String, Probe>,
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
            probes: HashMap::new(),
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
        let view = Self {
            app,
            new_id_input,
            new_url_input,
        };

        // 启动时立即测速，之后按固定周期自动重新测速。
        cx.spawn(async move |view, cx| {
            loop {
                let _ = view.update(cx, |view, cx| view.measure_all(cx));
                cx.background_executor().timer(PROBE_INTERVAL).await;
            }
        })
        .detach();

        view
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

    /// 对所有内置源与自定义源发起一轮分层测速。
    fn measure_all(&mut self, cx: &mut Context<Self>) {
        let builtins = registry::builtins();
        let builtin_ids: Vec<&str> = builtins.iter().map(|r| r.id).collect();
        let targets: Vec<(String, String)> = builtins
            .iter()
            .map(|r| (r.id.to_string(), r.url.to_string()))
            .chain(
                self.app
                    .read(cx)
                    .state
                    .customs
                    .iter()
                    .filter(|(id, _)| !builtin_ids.contains(&id.as_str()))
                    .map(|(id, url)| (id.clone(), url.clone())),
            )
            .collect();

        for (id, url) in targets {
            self.measure(id, url, cx);
        }
    }

    /// 对一个源执行四层探测，先置为进行中，完成后写入结果。
    fn measure(&mut self, id: String, url: String, cx: &mut Context<Self>) {
        self.app.update(cx, |model, cx| {
            model.state.probes.insert(id.clone(), Probe::running());
            cx.notify();
        });

        cx.spawn(async move |view, cx| {
            let report = cx
                .background_executor()
                .spawn(async move { net::probe(&url) })
                .await;
            let probe = Probe::from_report(report);

            let _ = view.update(cx, |view, cx| {
                view.app.update(cx, |model, cx| {
                    model.state.probes.insert(id.clone(), probe);
                    cx.notify();
                });
            });
        })
        .detach();
    }

    /// 单个源的四个步骤：步骤之间用贯通上下的分隔线隔开，并各自带状态 tips。
    fn render_probe(&self, id: &str, cx: &mut Context<Self>) -> AnyElement {
        let Some(probe) = self.app.read(cx).state.probes.get(id).cloned() else {
            return div().into_any_element();
        };

        let steps = [
            ("🔌", "TCP 连通", &probe.tcp),
            ("🌐", "HTTP 索引探测", &probe.http),
            ("📄", "索引文件解析", &probe.parse),
            ("⬇️", "下载端点速度", &probe.download),
        ];

        let mut row = h_flex().items_stretch().gap_3().self_stretch();
        for (index, (icon, title, level)) in steps.iter().enumerate() {
            if index > 0 {
                // 贯通列表项上下的分隔线：负外边距让它延伸进行内边距。
                row = row.child(
                    div()
                        .w(px(1.))
                        .self_stretch()
                        .mt(-px(12.))
                        .mb(-px(12.))
                        .bg(cx.theme().border),
                );
            }
            row = row.child(self.render_step(id, index, icon, title, level, cx));
        }
        row.into_any_element()
    }

    /// 渲染单个步骤：图标 + 状态/耗时文字，悬停显示状态说明。
    fn render_step(
        &self,
        row_id: &str,
        index: usize,
        icon: &'static str,
        title: &'static str,
        level: &Level,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (text, tone) = level_display(level);
        let color = match tone {
            LevelTone::Passed => cx.theme().success,
            LevelTone::Failed => cx.theme().danger,
            LevelTone::Pending => cx.theme().muted_foreground,
            LevelTone::Skipped => cx.theme().muted_foreground.alpha(0.35),
        };
        let tip = level_tip(title, level);

        v_flex()
            .justify_center()
            .id(format!("probe-{row_id}-{index}"))
            .tooltip(move |_, cx| cx.new(|_| Tooltip::new(tip.clone())).into())
            .child(
                Label::new(format!("{icon} {text}"))
                    .text_color(color)
                    .text_sm(),
            )
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
                    } else {
                        cx.theme().transparent
                    })
                    .border_1()
                    .border_color(if is_selected {
                        cx.theme().selection
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
                            .self_stretch()
                            .child(self.render_probe(r.id, cx))
                            .child({
                                if is_current {
                                    Label::new("✅").text_xl().into_any_element()
                                } else {
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
                                        }))
                                        .into_any_element()
                                }
                            }),
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
                    } else {
                        cx.theme().transparent
                    })
                    .border_1()
                    .border_color(if is_selected {
                        cx.theme().selection
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
                            .self_stretch()
                            .child(self.render_probe(&id, cx))
                            .child({
                                if is_current {
                                    Label::new("✅").text_xl().into_any_element()
                                } else {
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
                                        }))
                                        .into_any_element()
                                }
                            }),
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
                                model.state.selected = id.clone().into();
                                model.state.status = Status::Success("已添加自定义源".into());
                                cx.notify();
                            });
                            // 新增源立即触发一次分层测速。
                            if let Some(probe_url) = view.app.read(cx).state.customs.get(&id).cloned()
                            {
                                view.measure(id.clone(), probe_url, cx);
                            }
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

    fn probe(tcp: Level, http: Level, parse: Level, download: Level) -> Probe {
        Probe {
            tcp,
            http,
            parse,
            download,
        }
    }

    #[test]
    fn passed_step_shows_its_duration() {
        let (text, tone) = level_display(&Level::Passed("12 ms".into()));
        assert_eq!(text, "12 ms");
        assert_eq!(tone, LevelTone::Passed);
    }

    #[test]
    fn failed_step_is_red_and_truncated() {
        let long: SharedString = "x".repeat(30).into();
        let (text, tone) = level_display(&Level::Failed(long));
        assert_eq!(text, format!("{}…", "x".repeat(24)));
        assert_eq!(tone, LevelTone::Failed);
    }

    #[test]
    fn skipped_step_has_no_detail() {
        let (text, tone) = level_display(&Level::Skipped);
        assert_eq!(text, "—");
        assert_eq!(tone, LevelTone::Skipped);
    }

    #[test]
    fn running_step_shows_hourglass() {
        let (text, tone) = level_display(&Level::Running);
        assert_eq!(text, "⏳");
        assert_eq!(tone, LevelTone::Pending);
    }

    #[test]
    fn shorten_counts_chars_not_bytes() {
        assert_eq!(shorten("连接超时", 3), "连接超…");
        assert_eq!(shorten("abc", 3), "abc");
    }

    #[test]
    fn maps_net_layers_to_ui_levels() {
        assert_eq!(Level::from_layer(net::Layer::Skipped), Level::Skipped);
        assert_eq!(
            Level::from_layer(net::Layer::Passed("ok".into())),
            Level::Passed("ok".into())
        );
        assert_eq!(
            Level::from_layer(net::Layer::Failed("boom".into())),
            Level::Failed("boom".into())
        );
    }

    #[test]
    fn probe_helper_builds_all_layers() {
        let p = probe(
            Level::Passed("12 ms".into()),
            Level::Passed("30 ms".into()),
            Level::Passed("1.0.0 · 34 ms".into()),
            Level::Failed("timeout".into()),
        );
        assert_eq!(level_display(&p.tcp).1, LevelTone::Passed);
        assert_eq!(level_display(&p.download).1, LevelTone::Failed);
    }
}
