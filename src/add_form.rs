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
            logger::warn("custom", "添加自定义源失败：标识名或 URL 为空");
            view.app.update(cx, |model, cx| {
                model.state.status = Status::Error("标识名和 URL 都不能为空".into());
                cx.notify();
            });
            return;
        }
        if registry::builtins().iter().any(|r| r.id == id) {
            logger::warn("custom", format!("添加失败：「{id}」是内置源"));
            view.app.update(cx, |model, cx| {
                model.state.status =
                    Status::Error(format!("「{id}」是内置源，不能覆盖").into());
                cx.notify();
            });
            return;
        }
        if view.app.read(cx).state.customs.contains_key(&id) {
            logger::warn("custom", format!("添加失败：「{id}」已存在"));
            view.app.update(cx, |model, cx| {
                model.state.status =
                    Status::Error(format!("「{id}」已存在，请勿重复添加").into());
                cx.notify();
            });
            return;
        }
        let display_name = if name.is_empty() { id.clone() } else { name };
        let normalized_url = normalize_registry_url(&url);
        logger::info(
            "custom",
            format!("添加自定义源：{id} ({normalized_url})"),
        );
        view.app.update(cx, |model, cx| {
            model.state.customs.insert(
                id.clone(),
                CustomSource::new(display_name, normalized_url, description),
            );
            model.state.selected = id.clone().into();
            model.state.status = Status::Success("已添加自定义源".into());
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
                        .child(Label::new("标识名").text_sm())
                        .child(Input::new(&view.new_id_input).id("custom-id").text_color(cx.theme().foreground)),
                )
                .child(
                    v_flex()
                        .gap_1()
                        .flex_1()
                        .child(Label::new("名称").text_sm())
                        .child(Input::new(&view.new_name_input).id("custom-name").text_color(cx.theme().foreground)),
                )
                .child(
                    v_flex()
                        .gap_1()
                        .flex_grow(2.0)
                        .child(Label::new("Registry URL").text_sm())
                        .child(Input::new(&view.new_url_input).id("custom-url").text_color(cx.theme().foreground)),
                ),
        )
        .child(
            v_flex()
                .gap_1()
                .child(Label::new("说明（可选）").text_sm())
                .child(Input::new(&view.new_desc_input).id("custom-desc").text_color(cx.theme().foreground)),
        )
        .child(
            Button::new("confirm-add-custom")
                .label("确定")
                .primary()
                .w_full()
                .on_click(confirm_click),
        )
        .into_any()
}
