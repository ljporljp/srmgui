use std::time::Duration;

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{App, SharedString, WindowAppearance};

use crate::net;

/// Status line content + severity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Status {
    Info(SharedString),
    Success(SharedString),
    Error(SharedString),
}

/// 自动测速周期。
pub(crate) const PROBE_INTERVAL: Duration = Duration::from_secs(180);

/// 单层探测在 UI 中的状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Level {
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
pub(crate) struct Probe {
    pub(crate) tcp: Level,
    pub(crate) http: Level,
    pub(crate) parse: Level,
    pub(crate) download: Level,
}

impl Probe {
    /// 测速进行中的占位状态。
    pub(crate) fn running() -> Self {
        Self {
            tcp: Level::Running,
            http: Level::Running,
            parse: Level::Running,
            download: Level::Running,
        }
    }

    pub(crate) fn from_report(report: net::ProbeReport) -> Self {
        Self {
            tcp: Level::from_layer(report.tcp),
            http: Level::from_layer(report.http),
            parse: Level::from_layer(report.parse),
            download: Level::from_layer(report.download),
        }
    }
}

/// 生成探测结果的单行摘要，用于日志记录。
pub(crate) fn probe_summary(probe: &Probe) -> String {
    let skipped = rust_i18n::t!("probe.summary_skipped").to_string();
    let running = rust_i18n::t!("probe.summary_running").to_string();
    let level_text = |level: &Level| match level {
        Level::Passed(d) => d.to_string(),
        Level::Failed(e) => {
            rust_i18n::t!("probe.summary_failed", error = e.as_ref()).to_string()
        }
        Level::Skipped => skipped.clone(),
        Level::Running => running.clone(),
    };
    format!(
        "TCP={} | HTTP={} | {}={}",
        level_text(&probe.tcp),
        level_text(&probe.http),
        rust_i18n::t!("probe.summary_download"),
        level_text(&probe.download)
    )
}

/// 单个步骤的展示文字与语义状态。
///
/// 只保留耗时 / 速度指标（丢弃版本号等附加信息）；错误信息保留全文，
/// 由固定宽度布局截断，完整内容通过悬停 tips 查看。
pub(crate) fn level_display(level: &Level, speed_only: bool) -> (String, LevelTone) {
    match level {
        Level::Running => ("⏳".to_string(), LevelTone::Pending),
        Level::Skipped => ("—".to_string(), LevelTone::Skipped),
        Level::Passed(detail) => {
            let unit = if speed_only { "/s" } else { "ms" };
            let text = detail
                .split(" · ")
                .find(|part| part.contains(unit))
                .map(compact)
                .unwrap_or_else(|| compact(detail));
            (text, LevelTone::Passed)
        }
        Level::Failed(error) => (error.to_string(), LevelTone::Failed),
    }
}

/// 去掉指标中的空白，如 "125 KB/s" -> "125KB/s"。
fn compact(text: &str) -> String {
    text.split_whitespace().collect()
}

/// 步骤语义状态，用于决定文字颜色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LevelTone {
    Pending,
    Passed,
    Failed,
    Skipped,
}

/// 主题切换方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ThemeChoice {
    System,
    Light,
    Dark,
}

/// 界面语言。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Language {
    #[default]
    Zh,
    En,
}

impl Language {
    /// rust-i18n 使用的 locale 标识。
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Zh => "zh-CN",
            Self::En => "en-US",
        }
    }

    /// 从持久化的语言标识解析，无法识别时回退中文。
    pub(crate) fn from_code(code: &str) -> Self {
        if code.to_ascii_lowercase().starts_with("en") {
            Self::En
        } else {
            Self::Zh
        }
    }

    pub(crate) fn index(self) -> usize {
        match self {
            Self::Zh => 0,
            Self::En => 1,
        }
    }

    pub(crate) fn from_index(index: usize) -> Self {
        match index {
            1 => Self::En,
            _ => Self::Zh,
        }
    }
}

impl ThemeChoice {
    pub(crate) fn from_index(index: usize) -> Self {
        match index {
            1 => Self::Light,
            2 => Self::Dark,
            _ => Self::System,
        }
    }

    pub(crate) fn index(self) -> usize {
        match self {
            Self::System => 0,
            Self::Light => 1,
            Self::Dark => 2,
        }
    }
}

/// 应用主题：跟随系统时按当前系统外观解析，并同步原生窗口外观。
pub(crate) fn apply_theme(choice: ThemeChoice, cx: &mut App) {
    let appearance = match choice {
        ThemeChoice::System => None,
        ThemeChoice::Light => Some(WindowAppearance::Light),
        ThemeChoice::Dark => Some(WindowAppearance::Dark),
    };
    // 先解除/设置原生外观覆盖，再解析「跟随系统」的实际颜色。
    cx.set_window_appearance(appearance);
    let mode = match choice {
        ThemeChoice::System => ThemeMode::from(cx.window_appearance()),
        ThemeChoice::Light => ThemeMode::Light,
        ThemeChoice::Dark => ThemeMode::Dark,
    };
    Theme::change(mode, None, cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_choice_index_roundtrip() {
        for (index, choice) in [
            (0, ThemeChoice::System),
            (1, ThemeChoice::Light),
            (2, ThemeChoice::Dark),
        ] {
            assert_eq!(ThemeChoice::from_index(index), choice);
            assert_eq!(choice.index(), index);
        }
    }

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
        let (text, tone) = level_display(&Level::Passed("12 ms".into()), false);
        assert_eq!(text, "12ms");
        assert_eq!(tone, LevelTone::Passed);
    }

    #[test]
    fn passed_step_drops_version_and_keeps_duration() {
        let (text, _) = level_display(&Level::Passed("1.0.229 · 285 ms".into()), false);
        assert_eq!(text, "285ms");
    }

    #[test]
    fn download_step_shows_speed_only() {
        let (text, _) = level_display(&Level::Passed("651 ms · 125 KB/s".into()), true);
        assert_eq!(text, "125KB/s");
    }

    #[test]
    fn failed_step_keeps_full_error_for_tooltip() {
        let long: SharedString = "x".repeat(30).into();
        let (text, tone) = level_display(&Level::Failed(long.clone()), false);
        assert_eq!(text, long);
        assert_eq!(tone, LevelTone::Failed);
    }

    #[test]
    fn skipped_step_has_no_detail() {
        let (text, tone) = level_display(&Level::Skipped, false);
        assert_eq!(text, "—");
        assert_eq!(tone, LevelTone::Skipped);
    }

    #[test]
    fn running_step_shows_hourglass() {
        let (text, tone) = level_display(&Level::Running, false);
        assert_eq!(text, "⏳");
        assert_eq!(tone, LevelTone::Pending);
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
        assert_eq!(level_display(&p.tcp, false).1, LevelTone::Passed);
        assert_eq!(level_display(&p.download, true).1, LevelTone::Failed);
    }
}
