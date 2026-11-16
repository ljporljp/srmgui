use gpui_kit::AnyElement;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::component::label::Label;
use gpui_kit::component::{ActiveTheme, h_flex, v_flex};
use gpui_kit::prelude::*;

use crate::config_io;
use crate::config_io::CustomSource;
use crate::logger;
use crate::manifest;
use crate::model::Status;
use crate::root_view::RootView;
use crate::util::{normalize_registry_index, normalize_registry_url, valid_registry_name};

pub(crate) fn render_add_form(view: &RootView, cx: &mut Context<RootView>) -> AnyElement {
    let confirm_click = cx.listener(|view, _ev, window, cx| {
        let id: String = view.new_id_input.read(cx).value().trim().to_string();
        let name: String = view.new_name_input.read(cx).value().trim().to_string();
        let url: String = view.new_url_input.read(cx).value().trim().to_string();
        let description: String = view.new_desc_input.read(cx).value().trim().to_string();
        if id.is_empty() || url.is_empty() {
            logger::warn("custom", rust_i18n::t!("log.add_empty").to_string());
            view.app.update(cx, |model, cx| {
                model.state.status =
                    Status::Error(rust_i18n::t!("add.error_empty").to_string().into());
                cx.notify();
            });
            return;
        }
        if manifest::current_sources().iter().any(|r| r.id == id) {
            logger::warn(
                "custom",
                rust_i18n::t!("log.add_builtin", id = id.as_str()).to_string(),
            );
            view.app.update(cx, |model, cx| {
                model.state.status = Status::Error(
                    rust_i18n::t!("add.error_builtin", id = id)
                        .to_string()
                        .into(),
                );
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
                model.state.status = Status::Error(
                    rust_i18n::t!("add.error_exists", id = id)
                        .to_string()
                        .into(),
                );
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
            model.state.status = Status::Success(rust_i18n::t!("add.success").to_string().into());
            cx.notify();
        });
        let _ = config_io::write_customs(&view.app.read(cx).state.customs);
        if let Some(src) = view.app.read(cx).state.customs.get(&id).cloned() {
            view.measure(id.clone(), src.url, cx);
        }
        view.new_id_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        view.new_name_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        view.new_url_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        view.new_desc_input
            .update(cx, |s, cx| s.set_value("", window, cx));
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
                        .child(
                            Input::new(&view.new_id_input)
                                .id("custom-id")
                                .text_color(cx.theme().foreground),
                        ),
                )
                .child(
                    v_flex()
                        .gap_1()
                        .flex_1()
                        .child(Label::new(rust_i18n::t!("add.label_name").to_string()).text_sm())
                        .child(
                            Input::new(&view.new_name_input)
                                .id("custom-name")
                                .text_color(cx.theme().foreground),
                        ),
                )
                .child(
                    v_flex()
                        .gap_1()
                        .flex_grow(2.0)
                        .child(Label::new(rust_i18n::t!("add.label_url").to_string()).text_sm())
                        .child(
                            Input::new(&view.new_url_input)
                                .id("custom-url")
                                .text_color(cx.theme().foreground),
                        ),
                ),
        )
        .child(
            v_flex()
                .gap_1()
                .child(Label::new(rust_i18n::t!("add.label_description").to_string()).text_sm())
                .child(
                    Input::new(&view.new_desc_input)
                        .id("custom-desc")
                        .text_color(cx.theme().foreground),
                ),
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

/// 私有 registry 的添加表单：名称、index URL 与可选 token。
pub(crate) fn render_private_form(view: &RootView, cx: &mut Context<RootView>) -> AnyElement {
    let confirm_click = cx.listener(|view, _ev, window, cx| {
        let name: String = view
            .new_registry_name_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        let index: String = view
            .new_registry_index_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        let token: String = view
            .new_registry_token_input
            .read(cx)
            .value()
            .trim()
            .to_string();

        let set_error = |view: &mut RootView, message: String, cx: &mut Context<RootView>| {
            view.app.update(cx, |model, cx| {
                model.state.status = Status::Error(message.into());
                cx.notify();
            });
        };

        if name.is_empty() || index.is_empty() {
            logger::warn("registry", rust_i18n::t!("log.private_empty").to_string());
            set_error(view, rust_i18n::t!("private.error_empty").to_string(), cx);
            return;
        }
        if name == "crates-io" {
            set_error(
                view,
                rust_i18n::t!("private.error_reserved").to_string(),
                cx,
            );
            return;
        }
        if !valid_registry_name(&name) {
            set_error(view, rust_i18n::t!("private.error_name").to_string(), cx);
            return;
        }
        if view
            .app
            .read(cx)
            .state
            .registries
            .iter()
            .any(|registry| registry.name == name)
        {
            set_error(
                view,
                rust_i18n::t!("private.error_exists", name = name.as_str()).to_string(),
                cx,
            );
            return;
        }

        let (project_scope, project_root) = {
            let state = &view.app.read(cx).state;
            (state.project_scope, state.project_root.clone())
        };
        if project_scope && project_root.is_none() {
            set_error(
                view,
                rust_i18n::t!("status.project_required").to_string(),
                cx,
            );
            return;
        }

        let target = match config_io::config_path_scoped(if project_scope {
            project_root.as_deref()
        } else {
            None
        }) {
            Ok(path) => path,
            Err(error) => {
                set_error(
                    view,
                    rust_i18n::t!("status.private_failed", error = format!("{error:#}"))
                        .to_string(),
                    cx,
                );
                return;
            }
        };
        let scope = if project_scope {
            config_io::SCOPE_PROJECT
        } else {
            config_io::SCOPE_GLOBAL
        };
        let normalized = normalize_registry_index(&index);
        logger::info(
            "registry",
            rust_i18n::t!(
                "log.private_add",
                name = name.as_str(),
                index = normalized.as_str()
            )
            .to_string(),
        );

        match config_io::upsert_private_registry(
            &target,
            scope,
            &name,
            &normalized,
            Some(token.as_str()),
        ) {
            Ok(()) => {
                view.reload_registries(cx);
                view.app.update(cx, |model, cx| {
                    model.state.status = Status::Success(
                        rust_i18n::t!("status.private_added", name = name.as_str())
                            .to_string()
                            .into(),
                    );
                    cx.notify();
                });
                view.new_registry_name_input
                    .update(cx, |state, cx| state.set_value("", window, cx));
                view.new_registry_index_input
                    .update(cx, |state, cx| state.set_value("", window, cx));
                view.new_registry_token_input
                    .update(cx, |state, cx| state.set_value("", window, cx));
            }
            Err(error) => {
                logger::error(
                    "registry",
                    rust_i18n::t!("log.private_failed", error = format!("{error:#}")).to_string(),
                );
                set_error(
                    view,
                    rust_i18n::t!("status.private_failed", error = format!("{error:#}"))
                        .to_string(),
                    cx,
                );
            }
        }
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
                        .child(
                            Label::new(rust_i18n::t!("private.label_name").to_string()).text_sm(),
                        )
                        .child(
                            Input::new(&view.new_registry_name_input)
                                .id("private-name")
                                .text_color(cx.theme().foreground),
                        ),
                )
                .child(
                    v_flex()
                        .gap_1()
                        .flex_grow(2.0)
                        .child(
                            Label::new(rust_i18n::t!("private.label_index").to_string()).text_sm(),
                        )
                        .child(
                            Input::new(&view.new_registry_index_input)
                                .id("private-index")
                                .text_color(cx.theme().foreground),
                        ),
                ),
        )
        .child(
            v_flex()
                .gap_1()
                .child(Label::new(rust_i18n::t!("private.label_token").to_string()).text_sm())
                .child(
                    Input::new(&view.new_registry_token_input)
                        .id("private-token")
                        .mask_toggle()
                        .text_color(cx.theme().foreground),
                ),
        )
        .child(
            Button::new("confirm-add-private")
                .label(rust_i18n::t!("private.ok").to_string())
                .primary()
                .w_full()
                .on_click(confirm_click),
        )
        .into_any()
}
