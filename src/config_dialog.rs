//! 配置写入前的预览对话框，以及配置快照的一键回滚对话框。

use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::label::Label;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, Sizable, StyledExt, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, Entity, Hsla, Window, px};

use crate::config_io;
use crate::diff::{DiffLine, LineKind, diff_lines};
use crate::root_view::RootView;

/// 一次等待用户确认的配置写入。
#[derive(Debug, Clone)]
pub(crate) struct ApplyPlan {
    /// 目标配置文件路径。
    pub target: PathBuf,
    /// 现有文件内容，用于 diff。
    pub old: String,
    /// 合并后的新内容，用于 diff。
    pub new: String,
    /// 交给写入逻辑的 srmgui 托管内容。
    pub content: String,
    /// 选中的源标识。
    pub source: String,
    /// 是否写入项目级配置。
    pub project_scope: bool,
}

/// 写入前预览：展示合并后的 diff，确认后才真正落盘。
pub(crate) fn open_preview_dialog(
    window: &mut Window,
    cx: &mut App,
    view: Entity<RootView>,
    plan: ApplyPlan,
) {
    let target = plan.target.display().to_string();
    let unchanged = plan.old == plan.new;
    let lines = diff_lines(&plan.old, &plan.new);
    let added = lines.iter().filter(|line| line.kind == LineKind::Added).count();
    let removed = lines
        .iter()
        .filter(|line| line.kind == LineKind::Removed)
        .count();

    window.open_dialog(cx, move |dialog, _window, cx| {
        let muted = cx.theme().muted_foreground;
        let summary = if unchanged {
            rust_i18n::t!("preview.no_change").to_string()
        } else {
            rust_i18n::t!(
                "preview.summary",
                added = added.to_string(),
                removed = removed.to_string()
            )
            .to_string()
        };

        let view_for_ok = view.clone();
        let plan_for_ok = plan.clone();
        dialog
            .title(rust_i18n::t!("preview.title").to_string())
            .w(px(640.))
            .child(
                v_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        Label::new(
                            rust_i18n::t!("preview.target", path = target.as_str()).to_string(),
                        )
                        .text_sm()
                        .text_color(muted)
                        .whitespace_nowrap()
                        .text_ellipsis(),
                    )
                    .child(Label::new(summary).text_sm().font_semibold())
                    .child(
                        Label::new(rust_i18n::t!("preview.hint").to_string())
                            .text_sm()
                            .text_color(muted),
                    )
                    .child(
                        v_flex()
                            .id("preview-diff")
                            .w_full()
                            .max_h(px(320.))
                            .p_3()
                            .gap_0p5()
                            .rounded_md()
                            .border_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().muted)
                            .overflow_y_scrollbar()
                            .children(lines.iter().map(|line| {
                                diff_row(
                                    line,
                                    muted,
                                    cx.theme().success,
                                    cx.theme().danger,
                                )
                            })),
                    ),
            )
            .footer(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("preview-cancel")
                            .label(rust_i18n::t!("preview.cancel").to_string())
                            .secondary()
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("preview-ok")
                            .label(rust_i18n::t!("preview.ok").to_string())
                            .primary()
                            .on_click(move |_, window, cx| {
                                let plan = plan_for_ok.clone();
                                view_for_ok
                                    .update(cx, |view, cx| view.apply_confirmed(plan, cx));
                                window.close_dialog(cx);
                            }),
                    ),
            )
    });
}

/// 快照回滚：列出最近的配置快照，点击即可恢复。
pub(crate) fn open_snapshots_dialog(
    window: &mut Window,
    cx: &mut App,
    view: Entity<RootView>,
) {
    let snapshots = config_io::read_snapshots();

    window.open_dialog(cx, move |dialog, _window, cx| {
        let muted = cx.theme().muted_foreground;
        let body = if snapshots.is_empty() {
            v_flex().w_full().py_4().items_center().child(
                Label::new(
                    rust_i18n::t!("snapshot.empty", count = config_io::SNAPSHOT_LIMIT.to_string())
                        .to_string(),
                )
                .text_sm()
                .text_color(muted),
            )
            .into_any_element()
        } else {
            v_flex()
                .id("snapshot-list")
                .w_full()
                .max_h(px(320.))
                .gap_2()
                .overflow_y_scrollbar()
                .children(
                    snapshots
                        .iter()
                        .enumerate()
                        .map(|(index, snapshot)| snapshot_row(index, snapshot, view.clone(), cx)),
                )
                .into_any_element()
        };

        dialog
            .title(rust_i18n::t!("snapshot.title").to_string())
            .w(px(640.))
            .child(body)
            .footer(
                h_flex().w_full().justify_end().child(
                    Button::new("snapshot-close")
                        .label(rust_i18n::t!("snapshot.close").to_string())
                        .secondary()
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                ),
            )
    });
}

/// 一行 diff：新增 / 删除 / 上下文使用不同颜色与前缀。
fn diff_row(line: &DiffLine, muted: Hsla, success: Hsla, danger: Hsla) -> AnyElement {
    let (prefix, color) = match line.kind {
        LineKind::Context => (" ", muted),
        LineKind::Added => ("+", success),
        LineKind::Removed => ("-", danger),
    };
    Label::new(format!("{prefix} {}", line.text))
        .text_size(px(12.))
        .text_color(color)
        .into_any_element()
}

/// 一条快照记录：时间、作用域、源、目标路径与恢复按钮。
fn snapshot_row(
    index: usize,
    snapshot: &config_io::Snapshot,
    view: Entity<RootView>,
    cx: &App,
) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let scope = if snapshot.scope == config_io::SCOPE_PROJECT {
        rust_i18n::t!("scope.project").to_string()
    } else {
        rust_i18n::t!("scope.global").to_string()
    };
    let meta = rust_i18n::t!(
        "snapshot.meta",
        time = snapshot.created_at.as_str(),
        scope = scope,
        source = snapshot.source.as_str()
    )
    .to_string();
    let target = snapshot.target.display().to_string();
    let tip = rust_i18n::t!("snapshot.restore_tip").to_string();
    let snapshot_for_click = snapshot.clone();

    h_flex()
        .w_full()
        .items_center()
        .gap_3()
        .p_2()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().border)
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(Label::new(meta).text_sm().font_semibold())
                .child(
                    Label::new(target)
                        .text_sm()
                        .text_color(muted)
                        .whitespace_nowrap()
                        .text_ellipsis(),
                ),
        )
        .child(
            Button::new(format!("snapshot-restore-{index}"))
                .label(rust_i18n::t!("snapshot.restore").to_string())
                .small()
                .secondary()
                .tooltip(tip)
                .on_click(move |_, window, cx| {
                    let snapshot = snapshot_for_click.clone();
                    view.update(cx, |view, cx| view.restore_snapshot(snapshot, cx));
                    window.close_dialog(cx);
                }),
        )
        .into_any_element()
}
