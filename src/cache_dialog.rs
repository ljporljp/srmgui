//! Cargo 缓存管理对话框：展示 registry / git 缓存体积与无效缓存，并支持一键清理。

use gpui_kit::base::Disableable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::label::Label;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Sizable, StyledExt, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, Entity, SharedString, Window, div, px};

use crate::cache::{self, CacheReport, CacheState, Category, InvalidKind, SourceKind};
use crate::root_view::RootView;

/// 打开缓存管理对话框。
///
/// 对话框内容按 `RootView` 中的缓存状态实时渲染，统计与清理都在后台执行。
pub(crate) fn open_cache_dialog(window: &mut Window, cx: &mut App, view: Entity<RootView>) {
    window.open_dialog(cx, move |dialog, _window, cx| {
        let state = view.read(cx).app.read(cx).state.cache.clone();
        let busy = matches!(state, CacheState::Scanning | CacheState::Cleaning);
        let can_clean = matches!(&state, CacheState::Ready(report) if !report.invalid.is_empty());

        let view_for_refresh = view.clone();
        let view_for_clean = view.clone();

        dialog
            .title(rust_i18n::t!("cache.title").to_string())
            .w(px(720.))
            .child(render_body(&state, &view, cx))
            .footer(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("cache-refresh")
                            .label(rust_i18n::t!("cache.refresh").to_string())
                            .secondary()
                            .disabled(busy)
                            .on_click(move |_, _window, cx| {
                                let _ = view_for_refresh
                                    .update(cx, |view, cx| view.start_cache_scan(cx));
                            }),
                    )
                    .child(
                        Button::new("cache-clean")
                            .label(rust_i18n::t!("cache.clean").to_string())
                            .tooltip(rust_i18n::t!("cache.clean_tip").to_string())
                            .danger()
                            .disabled(!can_clean)
                            .on_click(move |_, _window, cx| {
                                let _ = view_for_clean.update(cx, |view, cx| view.clean_cache(cx));
                            }),
                    )
                    .child(
                        Button::new("cache-close")
                            .label(rust_i18n::t!("cache.close").to_string())
                            .secondary()
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    ),
            )
    });
}

/// 根据缓存状态渲染对话框主体。
fn render_body(state: &CacheState, view: &Entity<RootView>, cx: &App) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    match state {
        CacheState::Idle | CacheState::Scanning => v_flex()
            .w_full()
            .py_6()
            .items_center()
            .child(
                Label::new(rust_i18n::t!("cache.scanning").to_string())
                    .text_sm()
                    .text_color(muted),
            )
            .into_any_element(),
        CacheState::Cleaning => v_flex()
            .w_full()
            .py_6()
            .items_center()
            .child(
                Label::new(rust_i18n::t!("cache.cleaning").to_string())
                    .text_sm()
                    .text_color(muted),
            )
            .into_any_element(),
        CacheState::Failed(error) => v_flex()
            .w_full()
            .py_4()
            .items_center()
            .child(
                Label::new(rust_i18n::t!("cache.failed", error = error.as_str()).to_string())
                    .text_sm()
                    .text_color(cx.theme().danger),
            )
            .into_any_element(),
        CacheState::Ready(report) => render_report(report, view, cx),
    }
}

/// 扫描完成后的统计详情。
fn render_report(report: &CacheReport, view: &Entity<RootView>, cx: &App) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let home = report.home.display().to_string();
    let home_tip: SharedString = home.clone().into();

    let mut body = v_flex()
        .w_full()
        .gap_3()
        .child(
            div()
                .w_full()
                .id("cache-home")
                .tooltip(move |_, cx| cx.new(|_| Tooltip::new(home_tip.clone())).into())
                .child(
                    Label::new(rust_i18n::t!("cache.home", path = home.as_str()).to_string())
                        .w_full()
                        .text_sm()
                        .text_color(muted)
                        .whitespace_nowrap()
                        .text_ellipsis(),
                ),
        )
        .child(
            h_flex()
                .w_full()
                .items_center()
                .gap_2()
                .child(
                    Label::new(rust_i18n::t!("cache.total").to_string())
                        .text_sm()
                        .text_color(muted),
                )
                .child(
                    Label::new(cache::format_size(report.total_bytes()))
                        .text_lg()
                        .font_semibold(),
                ),
        );

    if report.sources.is_empty() {
        body = body.child(
            Label::new(rust_i18n::t!("cache.empty").to_string())
                .text_sm()
                .text_color(muted),
        );
    } else {
        if let Some(group) = render_sources(
            rust_i18n::t!("cache.registry").to_string(),
            report.registry_bytes,
            SourceKind::Registry,
            report,
            view,
            cx,
        ) {
            body = body.child(group);
        }
        if let Some(group) = render_sources(
            rust_i18n::t!("cache.git").to_string(),
            report.git_bytes,
            SourceKind::Git,
            report,
            view,
            cx,
        ) {
            body = body.child(group);
        }
    }

    body.child(render_invalid(report, cx))
        .child(
            Label::new(rust_i18n::t!("cache.invalid_hint").to_string())
                .text_sm()
                .text_color(muted),
        )
        .into_any_element()
}

/// 一类来源（registry / git）的标题与小计，以及每个来源的明细行。
fn render_sources(
    title: String,
    total: u64,
    kind: SourceKind,
    report: &CacheReport,
    view: &Entity<RootView>,
    cx: &App,
) -> Option<AnyElement> {
    let sources: Vec<&cache::Source> = report
        .sources
        .iter()
        .filter(|source| source.kind == kind)
        .collect();
    if sources.is_empty() {
        return None;
    }

    let muted = cx.theme().muted_foreground;
    Some(
        v_flex()
            .w_full()
            .gap_2()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .child(Label::new(title).text_sm().font_semibold())
                    .child(
                        Label::new(cache::format_size(total))
                            .text_sm()
                            .text_color(muted),
                    ),
            )
            .child(
                v_flex()
                    .id(format!("cache-sources-{kind:?}"))
                    .w_full()
                    .max_h(px(200.))
                    .gap_1()
                    .overflow_y_scrollbar()
                    .children(sources.into_iter().enumerate().map(|(index, source)| {
                        render_source(
                            format!("cache-source-{kind:?}-{index}"),
                            source,
                            &report.home,
                            view,
                            cx,
                        )
                    })),
            )
            .into_any_element(),
    )
}

/// 单个缓存来源：名称、各类别体积与体积小计。
fn render_source(
    id: String,
    source: &cache::Source,
    home: &std::path::Path,
    view: &Entity<RootView>,
    cx: &App,
) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let delete_id = format!("cache-delete-{id}");
    let target_home = home.to_path_buf();
    let target = source.clone();
    let view_for_delete = view.clone();
    let detail = source
        .parts
        .iter()
        .map(|part| {
            format!(
                "{} {}",
                category_label(part.category),
                cache::format_size(part.bytes)
            )
        })
        .collect::<Vec<_>>()
        .join(" · ");

    let mut tip = source
        .paths
        .iter()
        .map(|path| {
            path.strip_prefix(home)
                .unwrap_or(path)
                .display()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    if let Some(url) = &source.url {
        tip.push('\n');
        tip.push_str(url);
    }
    let tip: SharedString = tip.into();

    let mut row = v_flex()
        .w_full()
        .gap_1()
        .p_2()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().border)
        .child(
            h_flex()
                .w_full()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .id(id)
                        .tooltip(move |_, cx| cx.new(|_| Tooltip::new(tip.clone())).into())
                        .child(
                            Label::new(display_name(source))
                                .w_full()
                                .text_sm()
                                .font_semibold()
                                .whitespace_nowrap()
                                .text_ellipsis(),
                        ),
                )
                .child(
                    Label::new(cache::format_size(source.bytes))
                        .flex_shrink_0()
                        .text_sm()
                        .font_semibold(),
                )
                .child(
                    Button::new(delete_id)
                        .label(rust_i18n::t!("cache.delete_source").to_string())
                        .tooltip(rust_i18n::t!("cache.delete_source_tip").to_string())
                        .small()
                        .danger()
                        .on_click(move |_, _window, cx| {
                            let home = target_home.clone();
                            let source = target.clone();
                            let _ = view_for_delete
                                .update(cx, |view, cx| view.delete_cache_source(home, source, cx));
                        }),
                ),
        )
        .child(
            Label::new(detail)
                .w_full()
                .text_sm()
                .text_color(muted)
                .whitespace_nowrap()
                .text_ellipsis(),
        );

    if let Some(url) = &source.url {
        row = row.child(
            Label::new(url.clone())
                .w_full()
                .text_sm()
                .text_color(muted)
                .whitespace_nowrap()
                .text_ellipsis(),
        );
    }

    row.into_any_element()
}

/// 无效缓存区域：无问题时显示成功提示，否则列出待清理条目。
fn render_invalid(report: &CacheReport, cx: &App) -> AnyElement {
    if report.invalid.is_empty() {
        return v_flex()
            .w_full()
            .gap_2()
            .child(
                Label::new(rust_i18n::t!("cache.invalid_title").to_string())
                    .text_sm()
                    .font_semibold(),
            )
            .child(
                Label::new(rust_i18n::t!("cache.invalid_none").to_string())
                    .text_sm()
                    .text_color(cx.theme().success),
            )
            .into_any_element();
    }

    let summary = rust_i18n::t!(
        "cache.invalid_found",
        count = report.invalid.len().to_string(),
        size = cache::format_size(report.invalid_bytes())
    )
    .to_string();

    v_flex()
        .w_full()
        .gap_2()
        .child(
            Label::new(rust_i18n::t!("cache.invalid_title").to_string())
                .text_sm()
                .font_semibold(),
        )
        .child(Label::new(summary).text_sm().text_color(cx.theme().warning))
        .child(
            v_flex()
                .id("cache-invalid-list")
                .w_full()
                .max_h(px(180.))
                .gap_1()
                .overflow_y_scrollbar()
                .children(
                    report
                        .invalid
                        .iter()
                        .enumerate()
                        .map(|(index, entry)| invalid_row(index, entry, &report.home, cx)),
                ),
        )
        .into_any_element()
}

/// 一条无效缓存：类型、相对路径与体积。
fn invalid_row(
    index: usize,
    entry: &cache::InvalidEntry,
    home: &std::path::Path,
    cx: &App,
) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let kind = match entry.kind {
        InvalidKind::IncompleteSrc => {
            rust_i18n::t!("cache.invalid_kind.incomplete_src").to_string()
        }
        InvalidKind::OrphanCheckout => {
            rust_i18n::t!("cache.invalid_kind.orphan_checkout").to_string()
        }
        InvalidKind::OrphanRegistry => {
            rust_i18n::t!("cache.invalid_kind.orphan_registry").to_string()
        }
    };
    let path = entry
        .path
        .strip_prefix(home)
        .unwrap_or(&entry.path)
        .display()
        .to_string();
    let tip: SharedString = entry.path.display().to_string().into();

    h_flex()
        .w_full()
        .items_center()
        .gap_2()
        .child(
            Label::new(kind)
                .w(px(190.))
                .flex_shrink_0()
                .text_sm()
                .text_color(cx.theme().warning),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .id(format!("cache-invalid-{index}"))
                .tooltip(move |_, cx| cx.new(|_| Tooltip::new(tip.clone())).into())
                .child(
                    Label::new(path)
                        .w_full()
                        .text_sm()
                        .text_color(muted)
                        .whitespace_nowrap()
                        .text_ellipsis(),
                ),
        )
        .child(
            Label::new(cache::format_size(entry.bytes))
                .flex_shrink_0()
                .text_sm()
                .text_color(muted),
        )
        .into_any_element()
}

/// 来源标题：优先展示完整索引 URL / 仓库地址，无法还原时展示 cargo 目录标识。
fn display_name(source: &cache::Source) -> String {
    source.url.clone().unwrap_or_else(|| source.name.clone())
}

/// 缓存类别的界面名称。
fn category_label(category: Category) -> String {
    let key = match category {
        Category::RegistryIndex => "cache.category.index",
        Category::RegistryCache => "cache.category.cache",
        Category::RegistrySrc => "cache.category.src",
        Category::GitDb => "cache.category.db",
        Category::GitCheckouts => "cache.category.checkouts",
    };
    rust_i18n::t!(key).to_string()
}
