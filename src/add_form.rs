use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::component::label::Label;
use gpui_kit::component::{ActiveTheme, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::AnyElement;

use crate::config_io;
use crate::config_io::CustomSource;
use crate::logger;
use crate::model::Status;
use crate::registry;
use crate::root_view::RootView;
use crate::util::normalize_registry_url;

pub(crate) fn render_add_form(view: &RootView, cx: &mut Context<RootView>) -> AnyElement {
    let confirm_click = cx.listener(|view, _ev, window, cx| {
        let id: String = view.new_id_input.read(cx).value().trim().to_string();
        let name: String = view.new_name_input.read(cx).value().trim().to_string();
        let url: String = view.new_url_input.read(cx).value().trim().to_string();
        let description: String = view
            .new_desc_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        if id.is_empty() || url.is_empty() {
            logger::warn("custom", rust_i18n::t!("log.add_empty").to_string());
            view.app.update(cx, |model, cx| {
                model.state.status =
                    Status::Error(rust_i18n::t!("add.error_empty").to_string().into());
                cx.notify();
            });
            return;
        }
        if registry::builtins().iter().any(|r| r.id == id) {
            logger::warn(
                "custom",
                rust_i18n::t!("log.add_builtin", id = id.as_str()).to_string(),
            );
            view.app.update(cx, |model, cx| {
                model.state.status =
                    Status::Error(rust_i18n::t!("add.error_builtin", id = id).to_string().into());
                cx.notify();
            });
            return;
        }
        if view.app.read(cx).state.customs.contains_key(&id) {
            logger::warn(
                "custom",
                rust_i18n::t!("log.add_exists", id = id.as_str()).to_string(),
            );
            view.app.update(cx, |model, cx| {
                model.state.status =
                    Status::Error(rust_i18n::t!("add.error_exists", id = id).to_string().into());
                cx.notify();
            });
            return;
        }
        let display_name = if name.is_empty() { id.clone() } else { name };
        let normalized_url = normalize_registry_url(&url);
        logger::info(
            "custom",
            rust_i18n::t!("log.add", id = id.as_str(), url = normalized_url.as_str()).to_string(),
        );
        view.app.update(cx, |model, cx| {
            model.state.customs.insert(
                id.clone(),
                CustomSource::new(display_name, normalized_url, description),
            );
            model.state.selected = id.clone().into();
            model.state.status =
                Status::Success(rust_i18n::t!("add.success").to_string().into());
            cx.notify();
        });
        let _ = config_io::write_customs(&view.app.read(cx).state.customs);
        if let Some(src) = view.app.read(cx).state.customs.get(&id).cloned() {
            view.measure(id.clone(), src.url, cx);
        }
        view.new_id_input.update(cx, |s, cx| s.set_value("", window, cx));
        view.new_name_input.update(cx, |s, cx| s.set_value("", window, cx));
        view.new_url_input.update(cx, |s, cx| s.set_value("", window, cx));
        view.new_desc_input.update(cx, |s, cx| s.set_value("", window, cx));
    });

    v_flex()
        .gap_3()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(cx.theme().border)
        .child(
            h_flex()
                .gap_2()
                .child(
                    v_flex()
                        .gap_1()
                        .flex_1()
                        .child(Label::new(rust_i18n::t!("add.label_id").to_string()).text_sm())
                        .child(Input::new(&view.new_id_input).id("custom-id").text_color(cx.theme().foreground)),
                )
                .child(
                    v_flex()
                        .gap_1()
                        .flex_1()
                        .child(Label::new(rust_i18n::t!("add.label_name").to_string()).text_sm())
                        .child(Input::new(&view.new_name_input).id("custom-name").text_color(cx.theme().foreground)),
                )
                .child(
                    v_flex()
                        .gap_1()
                        .flex_grow(2.0)
                        .child(Label::new(rust_i18n::t!("add.label_url").to_string()).text_sm())
                        .child(Input::new(&view.new_url_input).id("custom-url").text_color(cx.theme().foreground)),
                ),
        )
        .child(
            v_flex()
                .gap_1()
                .child(Label::new(rust_i18n::t!("add.label_description").to_string()).text_sm())
                .child(Input::new(&view.new_desc_input).id("custom-desc").text_color(cx.theme().foreground)),
        )
        .child(
            Button::new("confirm-add-custom")
                .label(rust_i18n::t!("add.ok").to_string())
                .primary()
                .w_full()
                .on_click(confirm_click),
        )
        .into_any()
}
