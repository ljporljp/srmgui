use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::label::Label;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme, IconName, Sizable, StyledExt, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::*;
use gpui_kit::{
    Animation, AnimationExt as _, AnyElement, App, ClipboardItem, Hsla, Image, ImageFormat,
    SharedString, Window, div, img, px,
};

use crate::model::Level;

/// 每个探测步骤内容的固定展示宽度。
pub(crate) const STEP_WIDTH: f32 = 80.;

/// 源名称（含说明）列的固定宽度。
pub(crate) const NAME_WIDTH: f32 = 160.;

/// 跑马灯两段文字之间的间距。
const MARQUEE_GAP: f32 = 32.;

/// 粗估单行文字宽度（ASCII 按半个字宽计），用于判断是否会被截断。
fn text_width_estimate(text: &str, font_size: f32) -> f32 {
    text.chars()
        .map(|c| {
            if c.is_ascii() {
                font_size * 0.55
            } else {
                font_size
            }
        })
        .sum()
}

/// 固定宽度单行文本：放得下时省略号截断；放不下时循环滚动。
/// 悬停 tips 显示完整详情（可多于该行本身的内容）。
pub(crate) fn scrolling_text(
    id: String,
    text: SharedString,
    tip: SharedString,
    width: f32,
    font_size: f32,
    color: Hsla,
    semibold: bool,
) -> AnyElement {
    let text_width = text_width_estimate(&text, font_size);

    let body = if text_width <= width {
        Label::new(text)
            .w_full()
            .text_size(px(font_size))
            .text_color(color)
            .whitespace_nowrap()
            .text_ellipsis()
            .when(semibold, |l| l.font_semibold())
            .into_any_element()
    } else {
        // 复制一份文字并衔接排列，滚动一整段后视觉上无缝回到起点。
        let cycle = text_width + MARQUEE_GAP;
        let duration = Duration::from_secs_f32((cycle / 20.).clamp(4., 10.));
        let label = move |text: SharedString| {
            Label::new(text)
                .flex_shrink_0()
                .text_size(px(font_size))
                .text_color(color)
                .whitespace_nowrap()
                .when(semibold, |l| l.font_semibold())
        };
        h_flex()
            .gap(px(MARQUEE_GAP))
            .with_animation(
                format!("marquee-{id}"),
                Animation::new(duration).repeat(),
                move |this, delta| {
                    let progress = ((delta - 0.1) / 0.75).clamp(0., 1.);
                    this.ml(px(-cycle * progress))
                },
            )
            .child(label(text.clone()))
            .child(label(text))
            .into_any_element()
    };

    div()
        .w(px(width))
        .flex_shrink_0()
        .overflow_hidden()
        .id(format!("text-{id}"))
        .tooltip(move |_, cx| cx.new(|_| Tooltip::new(tip.clone())).into())
        .child(body)
        .into_any_element()
}

/// 应用图标：编译期内嵌，不依赖运行时资源文件。
///
/// 图标每帧都会被渲染，缓存为单例可避免重复解码与纹理重复上传。
pub(crate) fn logo_icon(size: f32) -> AnyElement {
    static LOGO: std::sync::OnceLock<std::sync::Arc<Image>> = std::sync::OnceLock::new();
    let image = LOGO.get_or_init(|| {
        std::sync::Arc::new(Image::from_bytes(
            ImageFormat::Png,
            include_bytes!("../assets/logo.png").to_vec(),
        ))
    });
    img(image.clone())
        .w(px(size))
        .h(px(size))
        .flex_shrink_0()
        .into_any_element()
}

/// 底部面板统一的滑入动画包装。
pub(crate) fn bottom_slide(child: AnyElement) -> AnyElement {
    div()
        .id("bottom-panel-slide")
        .with_animation(
            "bottom-panel-slide-in",
            Animation::new(Duration::from_millis(220)),
            |this, delta| this.opacity(delta).mt(px(16.0 * (1.0 - delta))),
        )
        .child(child)
        .into_any_element()
}

/// 「关于」模态窗口：展示应用名称、版本、说明与项目信息。
pub(crate) fn open_about_dialog(window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, |dialog, _window, cx| {
        let muted = cx.theme().muted_foreground;
        let (author, email) = split_author(env!("CARGO_PKG_AUTHORS"));
        dialog
            .title(rust_i18n::t!("about.title").to_string())
            .w(px(420.))
            .child(
                v_flex()
                    .w_full()
                    .items_center()
                    .gap_3()
                    .pt_2()
                    .child(logo_icon(56.))
                    .child(
                        Label::new(rust_i18n::t!("app.title").to_string())
                            .text_lg()
                            .font_semibold(),
                    )
                    .child(
                        Label::new(
                            rust_i18n::t!("about.version", version = env!("CARGO_PKG_VERSION"))
                                .to_string(),
                        )
                        .text_sm()
                        .text_color(muted),
                    )
                    .child(
                        Label::new(rust_i18n::t!("about.description").to_string())
                            .text_sm()
                            .text_color(muted)
                            .text_center(),
                    )
                    .child(
                        v_flex()
                            .w_full()
                            .gap_2()
                            .mt_2()
                            .p_3()
                            .rounded_md()
                            .border_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().muted)
                            .child(about_copy_row(
                                "about-copy-author",
                                rust_i18n::t!("about.author").to_string(),
                                author,
                                muted,
                            ))
                            .child(about_copy_row(
                                "about-copy-email",
                                rust_i18n::t!("about.email").to_string(),
                                email,
                                muted,
                            ))
                            .child(about_row(
                                rust_i18n::t!("about.license").to_string(),
                                env!("CARGO_PKG_LICENSE"),
                                muted,
                            ))
                            .child(
                                h_flex()
                                    .w_full()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        Label::new(rust_i18n::t!("about.homepage").to_string())
                                            .text_sm()
                                            .text_color(muted)
                                            .w(px(64.)),
                                    )
                                    .child(
                                        div()
                                            .id("about-repo-link")
                                            .flex_1()
                                            .text_sm()
                                            .text_color(cx.theme().link)
                                            .underline()
                                            .cursor_pointer()
                                            .child("github.com/ljporljp/srmgui")
                                            .on_click(|_, _, cx| {
                                                cx.open_url("https://github.com/ljporljp/srmgui")
                                            }),
                                    ),
                            ),
                    ),
            )
            .footer(
                h_flex().w_full().justify_end().child(
                    Button::new("about-close")
                        .label(rust_i18n::t!("about.ok").to_string())
                        .primary()
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                ),
            )
    });
}

/// 「关于」窗口中的信息行。
fn about_row(label: String, value: &'static str, muted: Hsla) -> AnyElement {
    h_flex()
        .w_full()
        .items_center()
        .gap_2()
        .child(Label::new(label).text_sm().text_color(muted).w(px(64.)))
        .child(div().flex_1().text_sm().child(value))
        .into_any_element()
}

/// 「关于」窗口中可单值复制的信息行：右侧按钮将该行内容写入剪贴板。
fn about_copy_row(id: &'static str, label: String, value: &'static str, muted: Hsla) -> AnyElement {
    h_flex()
        .w_full()
        .items_center()
        .gap_2()
        .child(Label::new(label).text_sm().text_color(muted).w(px(64.)))
        .child(div().flex_1().text_sm().child(value))
        .child(
            Button::new(id)
                .icon(IconName::Copy)
                .tooltip(rust_i18n::t!("about.copy_tip").to_string())
                .small()
                .ghost()
                .on_click(|_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(value.to_string()))
                }),
        )
        .into_any_element()
}

/// 拆分 `CARGO_PKG_AUTHORS`（`姓名 <邮箱>` 格式）为姓名与邮箱。
fn split_author(authors: &'static str) -> (&'static str, &'static str) {
    match (authors.find('<'), authors.find('>')) {
        (Some(start), Some(end)) if start < end => {
            (authors[..start].trim(), authors[start + 1..end].trim())
        }
        _ => (authors, ""),
    }
}

/// 贯通列表项上下的竖直分隔线（负外边距延伸进行内边距）。
pub(crate) fn v_separator(color: Hsla) -> AnyElement {
    div()
        .w(px(1.))
        .self_stretch()
        .mt(-px(12.))
        .mb(-px(12.))
        .bg(color)
        .into_any_element()
}

/// 步骤悬停提示：说明该步骤的含义与当前状态。
pub(crate) fn level_tip(title: &str, level: &Level) -> String {
    match level {
        Level::Running => rust_i18n::t!("probe.tip_running", title = title).to_string(),
        Level::Skipped => rust_i18n::t!("probe.tip_skipped", title = title).to_string(),
        Level::Passed(detail) => {
            rust_i18n::t!("probe.tip_passed", title = title, detail = detail.as_ref()).to_string()
        }
        Level::Failed(error) => {
            rust_i18n::t!("probe.tip_failed", title = title, error = error.as_ref()).to_string()
        }
    }
}
