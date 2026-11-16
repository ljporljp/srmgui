//! 配置写入前的预览对话框，以及配置快照的一键回滚对话框。

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::base::Disableable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::label::Label;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, Sizable, StyledExt, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, Entity, Hsla, Window, px};

use crate::config_io;
use crate::diff::{DiffLine, LineKind, diff_lines};
use crate::health;
use crate::manifest;
use crate::root_view::RootView;
use crate::update;

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

/// 快照回滚：列出最近的配置快照，可一键恢复或对比差异。
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
                    rust_i18n::t!("snapshot.empty", count = config_io::effective_config(&config_io::read_settings()).snapshot_limit.to_string())
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
                .children(snapshots.iter().enumerate().map(|(index, snapshot)| {
                    snapshot_row(index, snapshot, &snapshots, view.clone(), cx)
                }))
                .into_any_element()
        };

        dialog
            .title(rust_i18n::t!("snapshot.title").to_string())
            .w(px(720.))
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

/// 通用 diff 查看器：展示两段文本的行级差异（只读，无写入动作）。
pub(crate) fn open_diff_dialog(
    window: &mut Window,
    cx: &mut App,
    title: String,
    left_label: String,
    left: String,
    right_label: String,
    right: String,
) {
    let lines = diff_lines(&left, &right);
    let added = lines.iter().filter(|line| line.kind == LineKind::Added).count();
    let removed = lines
        .iter()
        .filter(|line| line.kind == LineKind::Removed)
        .count();

    let title_for_dialog = title.clone();
    window.open_dialog(cx, move |dialog, _window, cx| {
        let muted = cx.theme().muted_foreground;
        let summary = if left == right {
            rust_i18n::t!("diff.no_change").to_string()
        } else {
            rust_i18n::t!(
                "diff.summary",
                added = added.to_string(),
                removed = removed.to_string()
            )
            .to_string()
        };

        dialog
            .title(title_for_dialog.clone())
            .w(px(640.))
            .child(
                v_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        Label::new(
                                            rust_i18n::t!("diff.left").to_string(),
                                        )
                                        .text_sm()
                                        .text_color(muted),
                                    )
                                    .child(
                                        Label::new(left_label.clone())
                                            .text_sm()
                                            .font_semibold()
                                            .whitespace_nowrap()
                                            .text_ellipsis(),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        Label::new(
                                            rust_i18n::t!("diff.right").to_string(),
                                        )
                                        .text_sm()
                                        .text_color(muted),
                                    )
                                    .child(
                                        Label::new(right_label.clone())
                                            .text_sm()
                                            .font_semibold()
                                            .whitespace_nowrap()
                                            .text_ellipsis(),
                                    ),
                            ),
                    )
                    .child(Label::new(summary).text_sm().font_semibold())
                    .child(
                        v_flex()
                            .id("diff-content")
                            .w_full()
                            .max_h(px(360.))
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
                h_flex().w_full().justify_end().child(
                    Button::new("diff-close")
                        .label(rust_i18n::t!("diff.close").to_string())
                        .secondary()
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                ),
            )
    });
}

/// 发现新版本后让用户选择下载方式：系统下载工具 / 应用内下载。
pub(crate) fn open_update_dialog(
    window: &mut Window,
    cx: &mut App,
    view: Entity<RootView>,
    available: update::Available,
) {
    let version = available.version.clone();
    let asset_name = available.asset_name.clone();
    let asset_url = available.asset_url.clone();

    window.open_dialog(cx, move |dialog, _window, cx| {
        let muted = cx.theme().muted_foreground;
        let view_for_system = view.clone();
        let view_for_builtin = view.clone();
        let url_for_system = asset_url.clone();

        dialog
            .title(rust_i18n::t!("update.choose_title", version = version.as_str()).to_string())
            .w(px(480.))
            .child(
                v_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        Label::new(rust_i18n::t!("update.choose_hint").to_string())
                            .text_sm()
                            .text_color(muted),
                    )
                    .child(
                        Label::new(
                            rust_i18n::t!("update.asset", name = asset_name.as_str()).to_string(),
                        )
                        .text_sm()
                        .whitespace_nowrap()
                        .text_ellipsis(),
                    ),
            )
            .footer(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("update-cancel")
                            .label(rust_i18n::t!("update.cancel").to_string())
                            .secondary()
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("update-system-download")
                            .label(rust_i18n::t!("update.system_download").to_string())
                            .tooltip(rust_i18n::t!("update.system_tip").to_string())
                            .secondary()
                            .on_click(move |_, window, cx| {
                                let url = url_for_system.clone();
                                view_for_system.update(cx, |view, cx| {
                                    view.open_update_in_system(url, cx)
                                });
                                window.close_dialog(cx);
                            }),
                    )
                    .child(
                        Button::new("update-builtin-download")
                            .label(rust_i18n::t!("update.builtin_download").to_string())
                            .tooltip(rust_i18n::t!("update.builtin_tip").to_string())
                            .primary()
                            .on_click(move |_, window, cx| {
                                view_for_builtin
                                    .update(cx, |view, cx| view.start_update(cx));
                                window.close_dialog(cx);
                            }),
                    ),
            )
    });
}

/// 打开「导入分享链接」对话框：粘贴 srmgui 分享链接后导入其中的源。
pub(crate) fn open_import_link_dialog(
    window: &mut Window,
    cx: &mut App,
    view: Entity<RootView>,
) {
    // 输入框与错误提示必须在对话框构建器之外创建，否则每次重建都会清空内容。
    let input = cx.new(|cx| {
        InputState::new(window, cx).placeholder(rust_i18n::t!("share.placeholder").to_string())
    });
    let error: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    window.open_dialog(cx, move |dialog, _window, cx| {
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let input_for_ok = input.clone();
        let error_for_ok = error.clone();
        let view_for_ok = view.clone();
        let message = error.borrow().clone();

        let mut content = v_flex().w_full().gap_2();
        if let Some(message) = message {
            // 校验提示固定在顶部，避免用户看不到而误以为按钮无效。
            content = content.child(Label::new(message).text_sm().text_color(danger));
        }
        content = content
            .child(
                Label::new(rust_i18n::t!("share.hint").to_string())
                    .text_sm()
                    .text_color(muted),
            )
            .child(Input::new(&input).id("share-link-input"));

        dialog
            .title(rust_i18n::t!("share.title").to_string())
            .w(px(560.))
            .child(content)
            .footer(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("share-link-cancel")
                            .label(rust_i18n::t!("share.cancel").to_string())
                            .secondary()
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("share-link-import")
                            .label(rust_i18n::t!("share.ok").to_string())
                            .primary()
                            .on_click(move |_, window, cx| {
                                let text = input_for_ok.read(cx).text().to_string();
                                match manifest::parse_share_link(&text) {
                                    Ok(sources) => {
                                        view_for_ok.update(cx, |view, cx| {
                                            view.import_shared_sources(sources, cx);
                                        });
                                        window.close_dialog(cx);
                                    }
                                    Err(message) => {
                                        *error_for_ok.borrow_mut() = Some(message);
                                        cx.refresh_windows();
                                    }
                                }
                            }),
                    ),
            )
    });
}

/// 让用户从全部快照中挑选一份，与基准快照进行对比。
fn open_compare_picker(
    window: &mut Window,
    cx: &mut App,
    base: config_io::Snapshot,
    snapshots: Vec<config_io::Snapshot>,
) {
    let base_label = base.created_at.clone();
    window.open_dialog(cx, move |dialog, _window, cx| {
        let muted = cx.theme().muted_foreground;
        let hint = rust_i18n::t!(
            "snapshot.diff_picker_hint",
            time = base_label.as_str()
        )
        .to_string();

        dialog
            .title(rust_i18n::t!("snapshot.diff_picker_title").to_string())
            .w(px(640.))
            .child(
                v_flex()
                    .w_full()
                    .gap_2()
                    .child(Label::new(hint).text_sm().text_color(muted))
                    .child(
                        v_flex()
                            .id("compare-picker")
                            .w_full()
                            .max_h(px(320.))
                            .gap_2()
                            .overflow_y_scrollbar()
                            .children(snapshots.iter().enumerate().map(|(index, snapshot)| {
                                compare_picker_row(index, snapshot, &base, cx)
                            })),
                    ),
            )
            .footer(
                h_flex().w_full().justify_end().child(
                    Button::new("picker-cancel")
                        .label(rust_i18n::t!("snapshot.close").to_string())
                        .secondary()
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                ),
            )
    });
}

/// 对比选择器中的一行快照。
fn compare_picker_row(
    index: usize,
    snapshot: &config_io::Snapshot,
    base: &config_io::Snapshot,
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
    // 基准快照本身不可选，避免对比自己。
    let is_base = snapshot.target == base.target && snapshot.created_at == base.created_at;
    let snapshot_for_click = snapshot.clone();
    let base_for_click = base.clone();

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
            Button::new(format!("compare-select-{index}"))
                .label(rust_i18n::t!("snapshot.diff_select").to_string())
                .small()
                .secondary()
                .disabled(is_base)
                .on_click(move |_, window, cx| {
                    let base = base_for_click.clone();
                    let picked = snapshot_for_click.clone();
                    let title = rust_i18n::t!(
                        "snapshot.diff_title",
                        left = base.created_at.as_str(),
                        right = picked.created_at.as_str()
                    )
                    .to_string();
                    let left_label = base.created_at.clone();
                    let right_label = picked.created_at.clone();
                    window.close_dialog(cx);
                    open_diff_dialog(
                        window,
                        cx,
                        title,
                        left_label,
                        base.content,
                        right_label,
                        picked.content,
                    );
                }),
        )
        .into_any_element()
}

/// 配置健康检查：列出检测到的问题，并支持一键修复。
pub(crate) fn open_health_dialog(
    window: &mut Window,
    cx: &mut App,
    view: Entity<RootView>,
    report: health::HealthReport,
) {
    window.open_dialog(cx, move |dialog, _window, cx| {
        let muted = cx.theme().muted_foreground;
        let target = report.path.display().to_string();
        let healthy = report.is_healthy();
        let exists = report.exists;
        let summary = if !exists {
            rust_i18n::t!("health.not_exists").to_string()
        } else if healthy {
            rust_i18n::t!("health.ok").to_string()
        } else {
            rust_i18n::t!("health.found", count = report.issues.len().to_string()).to_string()
        };
        let summary_color = if healthy {
            cx.theme().success
        } else {
            cx.theme().warning
        };
        let issues = report.issues.clone();
        let report_for_fix = report.clone();
        let view_for_fix = view.clone();

        dialog
            .title(rust_i18n::t!("health.title").to_string())
            .w(px(640.))
            .child(
                v_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        Label::new(
                            rust_i18n::t!("health.target", path = target.as_str()).to_string(),
                        )
                        .text_sm()
                        .text_color(muted)
                        .whitespace_nowrap()
                        .text_ellipsis(),
                    )
                    .child(
                        Label::new(summary)
                            .text_sm()
                            .font_semibold()
                            .text_color(summary_color),
                    )
                    .when(!healthy, |column| {
                        column.child(
                            v_flex()
                                .id("health-issues")
                                .w_full()
                                .max_h(px(320.))
                                .gap_2()
                                .overflow_y_scrollbar()
                                .children(issues.iter().enumerate().map(|(index, issue)| {
                                    health_issue_row(index, issue, cx)
                                })),
                        )
                    })
                    .child(
                        Label::new(rust_i18n::t!("health.hint").to_string())
                            .text_sm()
                            .text_color(muted),
                    ),
            )
            .footer(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("health-close")
                            .label(rust_i18n::t!("health.close").to_string())
                            .secondary()
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("health-fix")
                            .label(rust_i18n::t!("health.fix").to_string())
                            .tooltip(rust_i18n::t!("health.fix_tip").to_string())
                            .primary()
                            .disabled(healthy || !exists)
                            .on_click(move |_, window, cx| {
                                let report = report_for_fix.clone();
                                view_for_fix.update(cx, |view, cx| view.fix_health(report, cx));
                                window.close_dialog(cx);
                            }),
                    ),
            )
    });
}

/// 健康检查结果中的一行问题。
fn health_issue_row(index: usize, issue: &health::Issue, cx: &App) -> AnyElement {
    let raw = issue.detail.clone();
    let (title, color) = match issue.kind {
        health::IssueKind::DuplicateTable => (
            rust_i18n::t!("health.kind.duplicate").to_string(),
            cx.theme().warning,
        ),
        health::IssueKind::DanglingReplace => (
            rust_i18n::t!("health.kind.dangling").to_string(),
            cx.theme().danger,
        ),
        health::IssueKind::OrphanSource => (
            rust_i18n::t!("health.kind.orphan").to_string(),
            cx.theme().warning,
        ),
        health::IssueKind::Syntax => (
            rust_i18n::t!("health.kind.syntax").to_string(),
            cx.theme().danger,
        ),
    };
    let detail = match issue.kind {
        health::IssueKind::DuplicateTable => {
            rust_i18n::t!("health.issue.duplicate", table = raw.as_str()).to_string()
        }
        health::IssueKind::DanglingReplace => {
            rust_i18n::t!("health.issue.dangling", name = raw.as_str()).to_string()
        }
        health::IssueKind::OrphanSource => {
            rust_i18n::t!("health.issue.orphan", name = raw.as_str()).to_string()
        }
        health::IssueKind::Syntax => {
            rust_i18n::t!("health.issue.syntax", error = raw.as_str()).to_string()
        }
    };

    v_flex()
        .id(format!("health-issue-{index}"))
        .w_full()
        .gap_1()
        .p_2()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().border)
        .child(
            Label::new(format!("{}. {}", index + 1, title))
                .text_sm()
                .font_semibold()
                .text_color(color),
        )
        .child(
            Label::new(detail)
                .text_sm()
                .text_color(cx.theme().muted_foreground),
        )
        .into_any_element()
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

/// 一条快照记录：时间、作用域、源、目标路径，以及恢复 / 对比当前 / 对比其它快照。
fn snapshot_row(
    index: usize,
    snapshot: &config_io::Snapshot,
    all_snapshots: &[config_io::Snapshot],
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
    let snapshot_for_restore = snapshot.clone();
    let snapshot_for_diff_current = snapshot.clone();
    let snapshot_for_picker = snapshot.clone();
    let all_for_picker: Vec<config_io::Snapshot> = all_snapshots.to_vec();

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
            Button::new(format!("snapshot-diff-current-{index}"))
                .label(rust_i18n::t!("snapshot.diff_current").to_string())
                .small()
                .secondary()
                .tooltip(rust_i18n::t!("snapshot.diff_current_tip").to_string())
                .on_click(move |_, window, cx| {
                    let snapshot = snapshot_for_diff_current.clone();
                    let current_label = rust_i18n::t!("snapshot.current_config").to_string();
                    let title = rust_i18n::t!(
                        "snapshot.diff_title",
                        left = snapshot.created_at.as_str(),
                        right = current_label.as_str()
                    )
                    .to_string();
                    let left_label = snapshot.created_at.clone();
                    let right_label = current_label;
                    // 读不到当前文件时按空内容处理，让 diff 展示整份快照。
                    let current = config_io::read_config_content(&snapshot.target)
                        .unwrap_or_default();
                    open_diff_dialog(
                        window,
                        cx,
                        title,
                        left_label,
                        snapshot.content,
                        right_label,
                        current,
                    );
                }),
        )
        .child(
            Button::new(format!("snapshot-diff-{index}"))
                .label(rust_i18n::t!("snapshot.diff").to_string())
                .small()
                .secondary()
                .tooltip(rust_i18n::t!("snapshot.diff_tip").to_string())
                .on_click(move |_, window, cx| {
                    open_compare_picker(
                        window,
                        cx,
                        snapshot_for_picker.clone(),
                        all_for_picker.clone(),
                    );
                }),
        )
        .child(
            Button::new(format!("snapshot-restore-{index}"))
                .label(rust_i18n::t!("snapshot.restore").to_string())
                .small()
                .secondary()
                .tooltip(tip)
                .on_click(move |_, window, cx| {
                    let snapshot = snapshot_for_restore.clone();
                    view.update(cx, |view, cx| view.restore_snapshot(snapshot, cx));
                    window.close_dialog(cx);
                }),
        )
        .into_any_element()
}
