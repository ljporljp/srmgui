use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_kit::component::label::Label;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, SharedString, px};

use crate::config_io;
use crate::logger;
use crate::root_view::RootView;
use crate::ui::{NAME_WIDTH, scrolling_text, v_separator};

/// 源列表中的一行：内置源与自定义源共用同一套渲染。
pub(crate) struct SourceItem {
    pub(crate) id: String,
    pub(crate) name: SharedString,
    pub(crate) detail: SharedString,
    pub(crate) tip: SharedString,
    pub(crate) name_size: f32,
    pub(crate) is_selected: bool,
    pub(crate) is_current: bool,
    pub(crate) deletable: bool,
}

/// 可复用的源列表：按 kind 生成元素 id 前缀，可选空态提示。
pub(crate) struct SourceList {
    container_id: &'static str,
    kind: &'static str,
    empty_hint: Option<SharedString>,
    items: Vec<SourceItem>,
}

impl SourceList {
    pub(crate) fn new(container_id: &'static str, kind: &'static str) -> Self {
        Self {
            container_id,
            kind,
            empty_hint: None,
            items: Vec::new(),
        }
    }

    pub(crate) fn empty_hint(mut self, hint: impl Into<SharedString>) -> Self {
        self.empty_hint = Some(hint.into());
        self
    }

    pub(crate) fn items(mut self, items: Vec<SourceItem>) -> Self {
        self.items = items;
        self
    }

    pub(crate) fn render(self, view: &RootView, cx: &mut Context<RootView>) -> AnyElement {
        let Self {
            container_id,
            kind,
            empty_hint,
            items,
        } = self;

        if items.is_empty() {
            if let Some(hint) = empty_hint {
                return v_flex()
                    .id(container_id)
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .child(Label::new(hint).text_color(cx.theme().muted_foreground))
                    .overflow_y_scrollbar()
                    .into_any_element();
            }
        }

        v_flex()
            .id(container_id)
            .w_full()
            .flex_1()
            .min_h_0()
            .gap_2()
            .children(
                items
                    .into_iter()
                    .map(|item| source_row(kind, item, view, cx)),
            )
            .overflow_y_scrollbar()
            .into_any_element()
    }
}

/// 单行渲染：删除按钮（仅自定义源）、名称列、测速区与右侧操作区。
fn source_row(
    kind: &'static str,
    item: SourceItem,
    view: &RootView,
    cx: &mut Context<RootView>,
) -> AnyElement {
    let SourceItem {
        id,
        name,
        detail,
        tip,
        name_size,
        is_selected,
        is_current,
        deletable,
    } = item;

    let mut row = h_flex()
        .id(format!("{kind}-{id}"))
        .gap_2()
        .items_stretch()
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
        .cursor_pointer();

    if deletable {
        row = row.child(
            h_flex()
                .self_stretch()
                .flex_shrink_0()
                .items_center()
                .child({
                    let del_variant = ButtonCustomVariant::new(cx)
                        .color(cx.theme().transparent)
                        .foreground(cx.theme().foreground)
                        .hover(cx.theme().danger)
                        .active(cx.theme().danger);
                    Button::new(format!("{kind}-del-{id}"))
                        .label(rust_i18n::t!("source.delete").to_string())
                        .custom(del_variant)
                        .border_1()
                        .border_color(cx.theme().danger)
                        .on_click(cx.listener({
                            let delete_id = id.clone();
                            move |view, _ev, _window, cx| {
                                logger::info(
                                    "custom",
                                    rust_i18n::t!("log.custom_delete", id = delete_id.as_str())
                                        .to_string(),
                                );
                                view.app.update(cx, |model, cx| {
                                    model.state.customs.remove(&delete_id);
                                    if model.state.selected.as_ref() == delete_id {
                                        // 选中的自定义源被删除后回到 crates.io 官方源，
                                        // 与首次启动的默认高亮保持一致。
                                        model.state.selected = "official".to_string().into();
                                    }
                                    cx.notify();
                                });
                                // 持久化删除后的自定义源
                                let _ = config_io::write_customs(&view.app.read(cx).state.customs);
                            }
                        }))
                }),
        );
    }

    row.child(
        v_flex()
            .w(px(NAME_WIDTH))
            .flex_shrink_0()
            .justify_center()
            .gap_1()
            .child(scrolling_text(
                format!("{kind}-name-{id}"),
                name,
                tip.clone(),
                NAME_WIDTH,
                name_size,
                cx.theme().foreground,
                true,
            ))
            .child(scrolling_text(
                format!("{kind}-detail-{id}"),
                detail,
                tip,
                NAME_WIDTH,
                14.,
                cx.theme().muted_foreground,
                false,
            )),
    )
    .child(v_separator(cx.theme().border))
    .child(view.render_probe(&id, cx))
    .child(
        h_flex()
            .ml_auto()
            .items_center()
            .gap_2()
            .self_stretch()
            .child({
                if is_current {
                    h_flex()
                        .items_center()
                        .gap_1()
                        .child(
                            Label::new(rust_i18n::t!("source.current").to_string())
                                .text_color(cx.theme().success)
                                .text_sm(),
                        )
                        .child(Label::new("✅").text_xl())
                        .into_any_element()
                } else {
                    Button::new(format!("{kind}-apply-{id}"))
                        .label(rust_i18n::t!("source.apply").to_string())
                        .when(is_selected, |b| b.primary().large())
                        .when(!is_selected, |b| b.secondary())
                        .on_click(cx.listener({
                            let apply_id = id.clone();
                            move |view, _ev, window, cx| {
                                view.app.update(cx, |model, cx| {
                                    model.state.selected = apply_id.clone().into();
                                    cx.notify();
                                });
                                view.apply(window, cx);
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
}
