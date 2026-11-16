use std::path::Path;

use gpui_kit::component::label::Label;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, div, px};

use crate::logger::{self, LogLevel};
use crate::root_view::RootView;
use crate::util::open_in_file_manager;

/// 渲染底部滑出的日志面板，高度最多占窗口的 50%。
pub(crate) fn render_log_panel(viewport_height: f32, cx: &mut Context<RootView>) -> AnyElement {
    let max_height = viewport_height * 0.5;
    let entries = logger::logger().entries();
    let log_dir = logger::logger().dir().display().to_string();

    let body = if entries.is_empty() {
        div()
            .child(
                Label::new(rust_i18n::t!("log.empty").to_string())
                    .text_color(cx.theme().muted_foreground)
                    .text_sm(),
            )
            .into_any_element()
    } else {
        // 最新的日志显示在最下方，自动滚动到底部由 overflow_y_scrollbar 处理。
        v_flex()
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .p_2()
            .gap_1()
            .children(entries.iter().map(|entry| {
                let color = match entry.level {
                    LogLevel::Error => cx.theme().danger,
                    LogLevel::Warn => cx.theme().warning,
                    LogLevel::Info => cx.theme().foreground,
                    LogLevel::Debug => cx.theme().muted_foreground,
                };
                Label::new(entry.format())
                    .text_size(px(12.))
                    .text_color(color)
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .into_any_element()
            }))
            .into_any_element()
    };

    v_flex()
        .id("log-panel")
        .w_full()
        .h(px(max_height))
        .border_1()
        .border_color(cx.theme().border)
        .rounded_lg()
        .overflow_hidden()
        .child(
            h_flex()
                .p_2()
                .gap_2()
                .items_center()
                .bg(cx.theme().muted)
                .border_b_1()
                .border_color(cx.theme().border)
                .child(
                    Label::new(rust_i18n::t!("log.title").to_string())
                        .text_color(cx.theme().foreground)
                        .font_semibold(),
                )
                .child({
                    let log_dir_for_click = log_dir.clone();
                    div()
                        .id("log-dir-link")
                        .cursor_pointer()
                        .hover(|style| style.underline())
                        .tooltip(move |_, cx| {
                            cx.new(|_| Tooltip::new(rust_i18n::t!("log.open").to_string()))
                                .into()
                        })
                        .on_click(cx.listener(move |_view, _ev, _window, _cx| {
                            open_in_file_manager(Path::new(&log_dir_for_click));
                        }))
                        .child(
                            Label::new(
                                rust_i18n::t!("log.dir", dir = log_dir.as_str()).to_string(),
                            )
                            .text_color(cx.theme().muted_foreground)
                            .text_sm(),
                        )
                }),
        )
        .child(body)
        .into_any_element()
}
