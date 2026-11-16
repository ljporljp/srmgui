use std::path::Path;

use gpui_kit::base::Selectable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::component::label::Label;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, div, px, rgb};

use crate::logger::{self, LogEntry, LogLevel};
use crate::root_view::RootView;
use crate::util::open_in_file_manager;

/// 日志级别过滤：级别之间可多选，`全部` 与其他级别互斥。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LogLevelFilter {
    /// 是否处于「全部」状态。
    all: bool,
    /// 已选级别位掩码，`全部` 状态下为 0。
    levels: u8,
}

impl LogLevelFilter {
    /// 四个级别全部选中的位掩码。
    const ALL_LEVELS: u8 = 0b1111;

    /// 默认的「全部」状态。
    pub(crate) const fn all() -> Self {
        Self {
            all: true,
            levels: 0,
        }
    }

    /// 每个级别对应的位。
    fn bit(level: LogLevel) -> u8 {
        match level {
            LogLevel::Debug => 1,
            LogLevel::Info => 2,
            LogLevel::Warn => 4,
            LogLevel::Error => 8,
        }
    }

    /// 是否选中「全部」。
    pub(crate) fn is_all(self) -> bool {
        self.all
    }

    /// 该级别按钮是否处于选中状态（「全部」状态下只高亮「全部」）。
    fn selected(self, level: LogLevel) -> bool {
        !self.all && self.levels & Self::bit(level) != 0
    }

    /// 切换某个级别的选中状态；选中「全部」时点击级别会转为只选中该级别。
    ///
    /// 四个级别全部选中时自动归一为「全部」；全部取消时保持为空结果。
    fn toggle(&mut self, level: LogLevel) {
        if self.all {
            self.all = false;
            self.levels = Self::bit(level);
        } else {
            self.levels ^= Self::bit(level);
            if self.levels == Self::ALL_LEVELS {
                self.all = true;
                self.levels = 0;
            }
        }
    }

    /// 当前过滤条件是否接受该级别。
    fn allows(self, level: LogLevel) -> bool {
        self.all || self.levels & Self::bit(level) != 0
    }
}

/// 按级别与关键字过滤日志，关键字不区分大小写并匹配整行内容。
pub(crate) fn filter_entries(
    entries: &[LogEntry],
    filter: LogLevelFilter,
    keyword: &str,
) -> Vec<LogEntry> {
    let needle = keyword.trim().to_lowercase();
    entries
        .iter()
        .filter(|entry| {
            filter.allows(entry.level)
                && (needle.is_empty() || entry.format().to_lowercase().contains(&needle))
        })
        .cloned()
        .collect()
}

/// 级别按钮的稳定元素 ID。
fn level_button_id(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Debug => "log-level-debug",
        LogLevel::Info => "log-level-info",
        LogLevel::Warn => "log-level-warn",
        LogLevel::Error => "log-level-error",
    }
}

/// 渲染底部滑出的日志面板，高度最多占窗口的 50%。
pub(crate) fn render_log_panel(
    view: &RootView,
    viewport_height: f32,
    cx: &mut Context<RootView>,
) -> AnyElement {
    let max_height = viewport_height * 0.5;
    let keyword = view.log_search_input.read(cx).value().trim().to_string();
    let all_entries = logger::logger().entries();
    let no_entries_at_all = all_entries.is_empty();
    let entries = filter_entries(&all_entries, view.log_level, &keyword);
    let log_dir = logger::logger().dir().display().to_string();

    // 级别过滤选中态：柔和的蓝色，深浅主题下都醒目但不刺眼
    let selected_style = {
        let (bg, border, fg) = if cx.theme().is_dark() {
            (rgb(0x1e3a8a), rgb(0x3b82f6), rgb(0xdbeafe))
        } else {
            (rgb(0xdbeafe), rgb(0x60a5fa), rgb(0x1d4ed8))
        };
        move |button: Button| button.bg(bg).border_color(border).text_color(fg)
    };

    // 级别过滤按钮：全部与其他级别互斥，级别之间可多选。
    let mut level_buttons = h_flex().gap_1().items_center();
    level_buttons = level_buttons.child(
        Button::new("log-level-all")
            .label(rust_i18n::t!("log.filter_all").to_string())
            .small()
            .selected(view.log_level.is_all())
            .when(view.log_level.is_all(), selected_style)
            .on_click(cx.listener(|view, _ev, _window, cx| {
                view.log_level = LogLevelFilter::all();
                cx.notify();
            })),
    );
    for level in [
        LogLevel::Debug,
        LogLevel::Info,
        LogLevel::Warn,
        LogLevel::Error,
    ] {
        let selected = view.log_level.selected(level);
        level_buttons = level_buttons.child(
            Button::new(level_button_id(level))
                .label(level.as_str().to_string())
                .small()
                .selected(selected)
                .when(selected, selected_style)
                .on_click(cx.listener(move |view, _ev, _window, cx| {
                    view.log_level.toggle(level);
                    cx.notify();
                })),
        );
    }

    let body = if entries.is_empty() {
        let hint = if no_entries_at_all {
            rust_i18n::t!("log.empty").to_string()
        } else {
            rust_i18n::t!("log.filtered_empty").to_string()
        };
        div()
            .child(
                Label::new(hint)
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
        .bg(cx.theme().background)
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
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
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
                            .w_full()
                            .text_color(cx.theme().muted_foreground)
                            .text_sm()
                            .whitespace_nowrap()
                            .text_ellipsis(),
                        )
                })
                .child(
                    Button::new("export-diagnostics")
                        .label(rust_i18n::t!("log.export_diag").to_string())
                        .tooltip(rust_i18n::t!("log.export_diag_tip").to_string())
                        .small()
                        .secondary()
                        .on_click(cx.listener(|view, _ev, _window, cx| {
                            view.export_diagnostics(cx);
                        })),
                ),
        )
        .child(
            h_flex()
                .p_2()
                .gap_2()
                .items_center()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(level_buttons)
                .child(
                    div().flex_1().min_w_0().child(
                        Input::new(&view.log_search_input)
                            .id("log-search")
                            .text_color(cx.theme().foreground),
                    ),
                ),
        )
        .child(body)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(level: LogLevel, message: &str) -> LogEntry {
        LogEntry {
            timestamp: "2026-11-16 10:00:00.000".to_string(),
            level,
            module: "probe".to_string(),
            message: message.to_string(),
        }
    }

    #[test]
    fn level_filter_defaults_to_all() {
        let entries = vec![
            entry(LogLevel::Debug, "debug"),
            entry(LogLevel::Info, "info"),
            entry(LogLevel::Warn, "warn"),
            entry(LogLevel::Error, "error"),
        ];

        let all = filter_entries(&entries, LogLevelFilter::all(), "");
        assert_eq!(all.len(), 4);
    }

    #[test]
    fn levels_can_be_multi_selected_and_all_is_exclusive() {
        let entries = vec![
            entry(LogLevel::Debug, "debug"),
            entry(LogLevel::Info, "info"),
            entry(LogLevel::Warn, "warn"),
            entry(LogLevel::Error, "error"),
        ];

        // 从「全部」点击 WARN：只选中 WARN
        let mut filter = LogLevelFilter::all();
        filter.toggle(LogLevel::Warn);
        assert!(!filter.is_all());
        assert_eq!(filter_entries(&entries, filter, "").len(), 1);

        // 追加 ERROR：WARN + ERROR 多选
        filter.toggle(LogLevel::Error);
        let matched = filter_entries(&entries, filter, "");
        assert_eq!(matched.len(), 2);
        assert_eq!(matched[0].level, LogLevel::Warn);
        assert_eq!(matched[1].level, LogLevel::Error);

        // 取消 WARN：只剩 ERROR
        filter.toggle(LogLevel::Warn);
        assert_eq!(filter_entries(&entries, filter, "").len(), 1);

        // 取消最后一个级别：保持为空，不自动回到「全部」
        filter.toggle(LogLevel::Error);
        assert!(!filter.is_all());
        assert!(filter_entries(&entries, filter, "").is_empty());
    }

    #[test]
    fn selecting_every_level_collapses_to_all() {
        let entries = vec![
            entry(LogLevel::Debug, "debug"),
            entry(LogLevel::Info, "info"),
            entry(LogLevel::Warn, "warn"),
            entry(LogLevel::Error, "error"),
        ];

        // 从「全部」进入单选后逐个补齐四个级别
        let mut filter = LogLevelFilter::all();
        filter.toggle(LogLevel::Debug);
        assert!(!filter.is_all());
        filter.toggle(LogLevel::Info);
        filter.toggle(LogLevel::Warn);
        assert!(!filter.is_all());
        filter.toggle(LogLevel::Error);

        // 四个级别全选后自动归一为「全部」
        assert!(filter.is_all());
        assert_eq!(filter_entries(&entries, filter, "").len(), 4);
    }

    #[test]
    fn keyword_filter_is_case_insensitive() {
        let entries = vec![
            entry(LogLevel::Info, "Probe started for RsProxy"),
            entry(LogLevel::Error, "write failed"),
        ];

        let matched = filter_entries(&entries, LogLevelFilter::all(), "rsproxy");
        assert_eq!(matched.len(), 1);
        assert!(matched[0].message.contains("RsProxy"));

        let matched = filter_entries(&entries, LogLevelFilter::all(), "  FAILED ");
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].level, LogLevel::Error);
    }

    #[test]
    fn keyword_and_level_filters_combine() {
        let entries = vec![
            entry(LogLevel::Info, "config written"),
            entry(LogLevel::Error, "config write failed"),
        ];
        let mut filter = LogLevelFilter::all();
        filter.toggle(LogLevel::Error);
        assert_eq!(filter_entries(&entries, filter, "config").len(), 1);
        assert!(filter_entries(&entries, filter, "unknown").is_empty());
    }
}
