//! 应用设置对话框：展示可配置项并支持修改/恢复默认。

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::label::Label;
use gpui_kit::component::{ActiveTheme, StyledExt, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, Entity, Window, div, px};

use crate::config_io::{AppConfig, effective_config, read_settings, write_settings};
use crate::logger;

/// 网络超时的允许区间（秒）。
const TIMEOUT_RANGE: (u64, u64) = (1, 3_600);
/// 测速下载上限的允许区间（KB）。
const DOWNLOAD_KB_RANGE: (u64, u64) = (1, 102_400);
/// 最大重定向次数的允许区间。
const REDIRECTS_RANGE: (u64, u64) = (1, 20);
/// 测速周期的允许区间（秒）。
const PROBE_RANGE: (u64, u64) = (10, 86_400);
/// 更新检查周期的允许区间（秒）。
const UPDATE_RANGE: (u64, u64) = (60, 604_800);
/// 快照保留数量的允许区间（份）。
const SNAPSHOT_RANGE: (u64, u64) = (1, 100);
/// 单个日志文件上限的允许区间（MB）。
const LOG_MB_RANGE: (u64, u64) = (1, 1_024);
/// 滚动日志保留数量的允许区间（个）。
const LOG_ROTATED_RANGE: (u64, u64) = (1, 50);
/// 内存日志条数的允许区间（条）。
const LOG_ENTRIES_RANGE: (u64, u64) = (100, 100_000);

/// 创建一个只接受十进制数字的输入框。
fn numeric_input(
    window: &mut Window,
    cx: &mut App,
    value: String,
    placeholder: String,
) -> Entity<InputState> {
    let input = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder(placeholder)
            .validate(|text, _| text.chars().all(|c| c.is_ascii_digit()))
    });
    input.update(cx, |state, cx| state.set_value(value, window, cx));
    input
}

/// 创建一个只接受合法 crate 名字符（字母、数字、`-`、`_`）的输入框。
fn crate_input(
    window: &mut Window,
    cx: &mut App,
    value: String,
    placeholder: String,
) -> Entity<InputState> {
    let input = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder(placeholder)
            .validate(|text, _| {
                text.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            })
    });
    input.update(cx, |state, cx| state.set_value(value, window, cx));
    input
}

/// 读取并校验一个数值输入框。
///
/// - 内容为空表示使用默认值，返回 `None`；
/// - 超出允许区间或无法解析时记录一条问题说明，同样返回 `None`。
fn read_number(
    input: &Entity<InputState>,
    label: &str,
    range: (u64, u64),
    problems: &mut Vec<String>,
    cx: &App,
) -> Option<u64> {
    let text = input.read(cx).text().to_string();
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    match text.parse::<u64>() {
        Ok(value) if (range.0..=range.1).contains(&value) => Some(value),
        _ => {
            problems.push(
                rust_i18n::t!(
                    "settings.out_of_range",
                    field = label,
                    min = range.0.to_string(),
                    max = range.1.to_string()
                )
                .to_string(),
            );
            None
        }
    }
}

/// 打开设置对话框。
///
/// `on_saved` 在设置成功写盘后调用，供调用方按新配置重建定时任务等。
pub(crate) fn open_settings_dialog(
    window: &mut Window,
    cx: &mut App,
    on_saved: impl Fn(&mut Window, &mut App) + 'static,
) {
    let current = read_settings();
    let defaults = AppConfig::default();
    let editing = effective_config(&current);

    // 输入框必须在对话框构建器之外创建：构建器可能因重建而反复执行，
    // 若在其中创建并写入初值，用户每次键入都会被重置，表现为无法输入。
    let timeout = numeric_input(
        window,
        cx,
        editing.timeout_secs.to_string(),
        defaults.timeout_secs.to_string(),
    );
    let download_kb = numeric_input(
        window,
        cx,
        editing.download_max_kb.to_string(),
        defaults.download_max_kb.to_string(),
    );
    let sample_crate = crate_input(
        window,
        cx,
        editing.sample_crate.clone(),
        defaults.sample_crate.clone(),
    );
    let redirects = numeric_input(
        window,
        cx,
        editing.max_redirects.to_string(),
        defaults.max_redirects.to_string(),
    );
    let probe_int = numeric_input(
        window,
        cx,
        editing.probe_interval_secs.to_string(),
        defaults.probe_interval_secs.to_string(),
    );
    let update_int = numeric_input(
        window,
        cx,
        editing.update_interval_secs.to_string(),
        defaults.update_interval_secs.to_string(),
    );
    let snap_limit = numeric_input(
        window,
        cx,
        editing.snapshot_limit.to_string(),
        defaults.snapshot_limit.to_string(),
    );
    let log_mb = numeric_input(
        window,
        cx,
        editing.max_log_mb.to_string(),
        defaults.max_log_mb.to_string(),
    );
    let log_rot = numeric_input(
        window,
        cx,
        editing.max_rotated_logs.to_string(),
        defaults.max_rotated_logs.to_string(),
    );
    let log_ent = numeric_input(
        window,
        cx,
        editing.max_log_entries.to_string(),
        defaults.max_log_entries.to_string(),
    );

    // 校验失败时的提示文案，保存成功或恢复默认后清空。
    let error: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    // 对话框构建器每帧重建，回调用 Rc 共享。
    let on_saved: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(on_saved);

    window.open_dialog(cx, move |dialog, _window, cx| {
        let muted = cx.theme().muted_foreground;

        // 恢复默认：把各输入框重置为默认值并清除错误提示。
        let reset = {
            let (i_timeout, i_download, i_crate, i_redirects) = (
                timeout.clone(),
                download_kb.clone(),
                sample_crate.clone(),
                redirects.clone(),
            );
            let (i_probe, i_update, i_snap) =
                (probe_int.clone(), update_int.clone(), snap_limit.clone());
            let (i_log_mb, i_log_rot, i_log_ent) =
                (log_mb.clone(), log_rot.clone(), log_ent.clone());
            let d = defaults.clone();
            let error = error.clone();
            Button::new("settings-reset")
                .label(rust_i18n::t!("settings.reset").to_string())
                .secondary()
                .on_click(move |_, window, cx| {
                    i_timeout.update(cx, |s, cx| {
                        s.set_value(d.timeout_secs.to_string(), window, cx)
                    });
                    i_download.update(cx, |s, cx| {
                        s.set_value(d.download_max_kb.to_string(), window, cx)
                    });
                    i_crate.update(cx, |s, cx| s.set_value(d.sample_crate.clone(), window, cx));
                    i_redirects.update(cx, |s, cx| {
                        s.set_value(d.max_redirects.to_string(), window, cx)
                    });
                    i_probe.update(cx, |s, cx| {
                        s.set_value(d.probe_interval_secs.to_string(), window, cx)
                    });
                    i_update.update(cx, |s, cx| {
                        s.set_value(d.update_interval_secs.to_string(), window, cx)
                    });
                    i_snap.update(cx, |s, cx| {
                        s.set_value(d.snapshot_limit.to_string(), window, cx)
                    });
                    i_log_mb.update(cx, |s, cx| {
                        s.set_value(d.max_log_mb.to_string(), window, cx)
                    });
                    i_log_rot.update(cx, |s, cx| {
                        s.set_value(d.max_rotated_logs.to_string(), window, cx)
                    });
                    i_log_ent.update(cx, |s, cx| {
                        s.set_value(d.max_log_entries.to_string(), window, cx)
                    });
                    *error.borrow_mut() = None;
                    cx.refresh_windows();
                })
        };

        let cancel = Button::new("settings-cancel")
            .label(rust_i18n::t!("settings.cancel").to_string())
            .secondary()
            .on_click(|_, window, cx| window.close_dialog(cx));

        // 确定：逐项校验，全部合法才写盘；否则保留对话框并展示原因。
        let ok = {
            let i_timeout = timeout.clone();
            let i_download = download_kb.clone();
            let i_crate = sample_crate.clone();
            let i_redirects = redirects.clone();
            let i_probe = probe_int.clone();
            let i_update = update_int.clone();
            let i_snap = snap_limit.clone();
            let i_log_mb = log_mb.clone();
            let i_log_rot = log_rot.clone();
            let i_log_ent = log_ent.clone();
            let original = current.clone();
            let d = defaults.clone();
            let error = error.clone();
            let on_saved = on_saved.clone();

            Button::new("settings-ok")
                .label(rust_i18n::t!("settings.ok").to_string())
                .primary()
                .on_click(move |_, window, cx| {
                    let mut problems: Vec<String> = Vec::new();
                    let mut next = original.clone();

                    // 数值项：空输入表示沿用默认值，非空则必须落在允许区间内。
                    next.timeout_secs = read_number(
                        &i_timeout,
                        &rust_i18n::t!("settings.timeout").to_string(),
                        TIMEOUT_RANGE,
                        &mut problems,
                        cx,
                    )
                    .filter(|v| *v != d.timeout_secs);
                    next.max_redirects = read_number(
                        &i_redirects,
                        &rust_i18n::t!("settings.redirects").to_string(),
                        REDIRECTS_RANGE,
                        &mut problems,
                        cx,
                    )
                    .filter(|v| *v != d.max_redirects);
                    next.download_max_kb = read_number(
                        &i_download,
                        &rust_i18n::t!("settings.download_limit").to_string(),
                        DOWNLOAD_KB_RANGE,
                        &mut problems,
                        cx,
                    )
                    .filter(|v| *v != d.download_max_kb);
                    next.probe_interval_secs = read_number(
                        &i_probe,
                        &rust_i18n::t!("settings.probe_interval").to_string(),
                        PROBE_RANGE,
                        &mut problems,
                        cx,
                    )
                    .filter(|v| *v != d.probe_interval_secs);
                    next.update_interval_secs = read_number(
                        &i_update,
                        &rust_i18n::t!("settings.update_interval").to_string(),
                        UPDATE_RANGE,
                        &mut problems,
                        cx,
                    )
                    .filter(|v| *v != d.update_interval_secs);
                    next.snapshot_limit = read_number(
                        &i_snap,
                        &rust_i18n::t!("settings.snapshot_limit").to_string(),
                        SNAPSHOT_RANGE,
                        &mut problems,
                        cx,
                    )
                    .map(|v| v as usize)
                    .filter(|v| *v != d.snapshot_limit);
                    next.max_log_mb = read_number(
                        &i_log_mb,
                        &rust_i18n::t!("settings.log_size").to_string(),
                        LOG_MB_RANGE,
                        &mut problems,
                        cx,
                    )
                    .filter(|v| *v != d.max_log_mb);
                    next.max_rotated_logs = read_number(
                        &i_log_rot,
                        &rust_i18n::t!("settings.log_rotated").to_string(),
                        LOG_ROTATED_RANGE,
                        &mut problems,
                        cx,
                    )
                    .map(|v| v as usize)
                    .filter(|v| *v != d.max_rotated_logs);
                    next.max_log_entries = read_number(
                        &i_log_ent,
                        &rust_i18n::t!("settings.log_entries").to_string(),
                        LOG_ENTRIES_RANGE,
                        &mut problems,
                        cx,
                    )
                    .map(|v| v as usize)
                    .filter(|v| *v != d.max_log_entries);

                    // 示例 crate：空表示沿用默认值。
                    let krate = i_crate.read(cx).text().to_string();
                    let krate = krate.trim().to_string();
                    next.sample_crate = if krate.is_empty() || krate == d.sample_crate {
                        None
                    } else {
                        Some(krate)
                    };

                    if !problems.is_empty() {
                        *error.borrow_mut() = Some(problems.join("\n"));
                        cx.refresh_windows();
                        return;
                    }

                    // 写盘失败时同样保留对话框，并把原因显示在顶部提示条里。
                    if let Err(err) = write_settings(&next) {
                        *error.borrow_mut() = Some(
                            rust_i18n::t!("settings.save_failed", error = err.to_string())
                                .to_string(),
                        );
                        cx.refresh_windows();
                        return;
                    }
                    // 只有配置确实变化时才通知调用方重建定时任务、刷新测速。
                    let changed = next != original;
                    logger::info("settings", rust_i18n::t!("settings.saved").to_string());
                    *error.borrow_mut() = None;
                    if changed {
                        on_saved(window, cx);
                    }
                    window.close_dialog(cx);
                })
        };

        // 区块标题：左侧点缀色条 + 小号半粗标题。
        let sec = |text: String| -> AnyElement {
            h_flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .w(px(3.))
                        .h(px(12.))
                        .rounded_full()
                        .bg(cx.theme().primary),
                )
                .child(Label::new(text).text_xs().font_semibold().text_color(muted))
                .into_any_element()
        };

        // 统一的设置行：左侧标题与说明自适应宽度，右侧输入框与单位宽度固定，
        // 因此所有行的输入框右端严格对齐。
        let row = |lbl: String,
                   tip: String,
                   unit: &'static str,
                   id: &'static str,
                   input: Entity<InputState>|
         -> AnyElement {
            h_flex()
                .w_full()
                .items_center()
                .gap_4()
                .py_1()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0()
                        .child(Label::new(lbl).text_sm())
                        .child(Label::new(tip).text_xs().text_color(muted)),
                )
                .child(
                    h_flex()
                        .flex_shrink_0()
                        .items_center()
                        .gap_2()
                        .child(div().w(px(140.)).child(Input::new(&input).id(id)))
                        .child(
                            div()
                                .w(px(28.))
                                .child(Label::new(unit).text_xs().text_color(muted)),
                        ),
                )
                .into_any_element()
        };

        let divider = || div().h(px(1.0)).w_full().bg(cx.theme().border);

        // 校验提示固定放在内容顶部，避免落到滚动区底部而看不到。
        let error_banner: Option<AnyElement> = error.borrow().as_ref().map(|message| {
            Label::new(message.clone())
                .text_sm()
                .text_color(cx.theme().danger)
                .into_any_element()
        });

        let body = v_flex()
            .w_full()
            .gap_2()
            .children(error_banner)
            .child(sec(rust_i18n::t!("settings.network").to_string()))
            .child(row(
                rust_i18n::t!("settings.timeout").to_string(),
                rust_i18n::t!("settings.timeout_tip").to_string(),
                "秒",
                "settings-timeout",
                timeout.clone(),
            ))
            .child(row(
                rust_i18n::t!("settings.redirects").to_string(),
                rust_i18n::t!("settings.redirects_tip").to_string(),
                "次",
                "settings-redirects",
                redirects.clone(),
            ))
            .child(row(
                rust_i18n::t!("settings.sample_crate").to_string(),
                rust_i18n::t!("settings.sample_crate_tip").to_string(),
                "",
                "settings-sample-crate",
                sample_crate.clone(),
            ))
            .child(row(
                rust_i18n::t!("settings.download_limit").to_string(),
                rust_i18n::t!("settings.download_limit_tip").to_string(),
                "KB",
                "settings-download-kb",
                download_kb.clone(),
            ))
            .child(divider())
            .child(sec(rust_i18n::t!("settings.intervals").to_string()))
            .child(row(
                rust_i18n::t!("settings.probe_interval").to_string(),
                rust_i18n::t!("settings.probe_interval_tip").to_string(),
                "秒",
                "settings-probe",
                probe_int.clone(),
            ))
            .child(row(
                rust_i18n::t!("settings.update_interval").to_string(),
                rust_i18n::t!("settings.update_interval_tip").to_string(),
                "秒",
                "settings-update",
                update_int.clone(),
            ))
            .child(divider())
            .child(sec(rust_i18n::t!("settings.snapshot").to_string()))
            .child(row(
                rust_i18n::t!("settings.snapshot_limit").to_string(),
                rust_i18n::t!("settings.snapshot_limit_tip").to_string(),
                "份",
                "settings-snapshot",
                snap_limit.clone(),
            ))
            .child(divider())
            .child(sec(rust_i18n::t!("settings.logging").to_string()))
            .child(row(
                rust_i18n::t!("settings.log_size").to_string(),
                rust_i18n::t!("settings.log_size_tip").to_string(),
                "MB",
                "settings-log-mb",
                log_mb.clone(),
            ))
            .child(row(
                rust_i18n::t!("settings.log_rotated").to_string(),
                rust_i18n::t!("settings.log_rotated_tip").to_string(),
                "个",
                "settings-log-rotated",
                log_rot.clone(),
            ))
            .child(row(
                rust_i18n::t!("settings.log_entries").to_string(),
                rust_i18n::t!("settings.log_entries_tip").to_string(),
                "条",
                "settings-log-entries",
                log_ent.clone(),
            ));

        dialog
            .title(rust_i18n::t!("settings.title").to_string())
            .w(px(560.))
            .child(body)
            .footer(
                h_flex()
                    .w_full()
                    .justify_between()
                    .child(reset)
                    .child(h_flex().gap_2().child(cancel).child(ok)),
            )
    });
}
