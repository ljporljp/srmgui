use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_kit::component::label::Label;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, SharedString, px};

use crate::config_io;
use crate::logger;
use crate::model::Status;
use crate::root_view::RootView;
use crate::ui::{NAME_WIDTH, scrolling_text, v_separator};

/// 私有 registry 列表：展示名称、index URL 与 token 配置状态。
pub(crate) fn render_private_list(view: &RootView, cx: &mut Context<RootView>) -> AnyElement {
    let registries = view.app.read(cx).state.registries.clone();

    if registries.is_empty() {
        return v_flex()
            .id("private-list-scroll")
            .w_full()
            .flex_1()
            .min_h_0()
            .child(
                Label::new(rust_i18n::t!("private.empty").to_string())
                    .text_color(cx.theme().muted_foreground),
            )
            .overflow_y_scrollbar()
            .into_any_element();
    }

    v_flex()
        .id("private-list-scroll")
        .w_full()
        .flex_1()
        .min_h_0()
        .gap_2()
        .children(
            registries
                .into_iter()
                .map(|registry| registry_row(registry, cx)),
        )
        .overflow_y_scrollbar()
        .into_any_element()
}

/// 单行：删除按钮、名称/索引列、token 状态。
fn registry_row(registry: config_io::PrivateRegistry, cx: &mut Context<RootView>) -> AnyElement {
    let name = registry.name.clone();
    let index = registry.index.clone();
    let has_token = registry.has_token;
    let token_text = if has_token {
        rust_i18n::t!("private.token_yes").to_string()
    } else {
        rust_i18n::t!("private.token_no").to_string()
    };
    let tip: SharedString = rust_i18n::t!(
        "private.tip",
        name = name.as_str(),
        index = index.as_str(),
        token = token_text.as_str()
    )
    .to_string()
    .into();
    let tip_for_row = tip.clone();

    let del_variant = ButtonCustomVariant::new(cx)
        .color(cx.theme().transparent)
        .foreground(cx.theme().foreground)
        .hover(cx.theme().danger)
        .active(cx.theme().danger);

    h_flex()
        .id(format!("private-{name}"))
        .gap_2()
        .items_stretch()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(cx.theme().border)
        .child(
            h_flex()
                .self_stretch()
                .flex_shrink_0()
                .items_center()
                .child(
                    Button::new(format!("private-del-{name}"))
                        .label(rust_i18n::t!("source.delete").to_string())
                        .custom(del_variant)
                        .border_1()
                        .border_color(cx.theme().danger)
                        .on_click(cx.listener({
                            let delete_name = name.clone();
                            move |view, _ev, _window, cx| {
                                delete_registry(view, &delete_name, cx);
                            }
                        })),
                ),
        )
        .child(
            v_flex()
                .w(px(NAME_WIDTH))
                .flex_shrink_0()
                .justify_center()
                .gap_1()
                .child(scrolling_text(
                    format!("private-name-{name}"),
                    name.clone().into(),
                    tip.clone(),
                    NAME_WIDTH,
                    16.,
                    cx.theme().foreground,
                    true,
                ))
                .child(scrolling_text(
                    format!("private-index-{name}"),
                    index.clone().into(),
                    tip,
                    NAME_WIDTH,
                    14.,
                    cx.theme().muted_foreground,
                    false,
                )),
        )
        .child(v_separator(cx.theme().border))
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap_2()
                .id(format!("private-token-{name}"))
                .tooltip(move |_, cx| cx.new(|_| Tooltip::new(tip_for_row.clone())).into())
                .child(
                    Label::new(rust_i18n::t!("private.token_label").to_string())
                        .text_sm()
                        .text_color(cx.theme().muted_foreground),
                )
                .child(Label::new(token_text).text_sm().text_color(if has_token {
                    cx.theme().success
                } else {
                    cx.theme().muted_foreground
                })),
        )
        .into_any()
}

/// 删除私有 registry：同步清理 `[registries]` 配置与认证 token。
fn delete_registry(view: &mut RootView, name: &str, cx: &mut Context<RootView>) {
    let (target, scope) = match view.config_target(cx) {
        Ok(target) => target,
        Err(message) => {
            view.set_status(Status::Error(message.into()), cx);
            return;
        }
    };

    match config_io::remove_private_registry(&target, scope, name) {
        Ok(()) => {
            logger::info(
                "registry",
                rust_i18n::t!("log.private_delete", name = name).to_string(),
            );
            view.reload_registries(cx);
            view.set_status(
                Status::Success(
                    rust_i18n::t!("status.private_deleted", name = name)
                        .to_string()
                        .into(),
                ),
                cx,
            );
        }
        Err(error) => {
            logger::error(
                "registry",
                rust_i18n::t!("log.private_failed", error = format!("{error:#}")).to_string(),
            );
            view.set_status(
                Status::Error(
                    rust_i18n::t!("status.private_failed", error = format!("{error:#}"))
                        .to_string()
                        .into(),
                ),
                cx,
            );
        }
    }
}
