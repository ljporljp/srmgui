#![cfg_attr(windows, windows_subsystem = "windows")]

mod config_io;
mod logger;
mod net;
mod registry;

use logger::LogLevel;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use config_io::CustomSource;
use gpui_kit::PathPromptOptions;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::label::Label;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::component::{IconName, Theme, ThemeMode, TitleBar, WindowExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{
    Animation, AnimationExt as _, AnyElement, App, Entity, Hsla, Image, ImageFormat, SharedString,
    Window, WindowAppearance, WindowOptions, actions, div, img, px,
};

actions!(cargo_mirror_switcher, [Quit, ToggleTheme]);

/// Status line content + severity.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    Info(SharedString),
    Success(SharedString),
    Error(SharedString),
}

/// 自动测速周期。
const PROBE_INTERVAL: Duration = Duration::from_secs(180);

/// 每个探测步骤内容的固定展示宽度。
const STEP_WIDTH: f32 = 80.;

/// 源名称（含说明）列的固定宽度。
const NAME_WIDTH: f32 = 160.;

/// 跑马灯两段文字之间的间距。
const MARQUEE_GAP: f32 = 32.;

/// 单层探测在 UI 中的状态。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Level {
    /// 正在探测。
    Running,
    /// 前序层失败，未执行。
    Skipped,
    /// 通过，附带详情。
    Passed(SharedString),
    /// 失败，附带错误信息。
    Failed(SharedString),
}

impl Level {
    fn from_layer(layer: net::Layer) -> Self {
        match layer {
            net::Layer::Skipped => Level::Skipped,
            net::Layer::Passed(detail) => Level::Passed(detail.into()),
            net::Layer::Failed(error) => Level::Failed(error.into()),
        }
    }
}

/// 一个源的四层探测状态：TCP → HTTP 索引探测 → 索引文件解析 → 下载速度。
#[derive(Debug, Clone)]
struct Probe {
    tcp: Level,
    http: Level,
    parse: Level,
    download: Level,
}

impl Probe {
    /// 测速进行中的占位状态。
    fn running() -> Self {
        Self {
            tcp: Level::Running,
            http: Level::Running,
            parse: Level::Running,
            download: Level::Running,
        }
    }

    fn from_report(report: net::ProbeReport) -> Self {
        Self {
            tcp: Level::from_layer(report.tcp),
            http: Level::from_layer(report.http),
            parse: Level::from_layer(report.parse),
            download: Level::from_layer(report.download),
        }
    }
}

/// 生成探测结果的单行摘要，用于日志记录。
fn probe_summary(probe: &Probe) -> String {
    let tcp = match &probe.tcp {
        Level::Passed(d) => d.to_string(),
        Level::Failed(e) => format!("失败:{e}"),
        Level::Skipped => "跳过".into(),
        Level::Running => "进行中".into(),
    };
    let http = match &probe.http {
        Level::Passed(d) => d.to_string(),
        Level::Failed(e) => format!("失败:{e}"),
        Level::Skipped => "跳过".into(),
        Level::Running => "进行中".into(),
    };
    let download = match &probe.download {
        Level::Passed(d) => d.to_string(),
        Level::Failed(e) => format!("失败:{e}"),
        Level::Skipped => "跳过".into(),
        Level::Running => "进行中".into(),
    };
    format!("TCP={tcp} | HTTP={http} | 下载={download}")
}

/// 单个步骤的展示文字与语义状态。
///
/// 只保留耗时 / 速度指标（丢弃版本号等附加信息）；错误信息保留全文，
/// 由固定宽度布局截断，完整内容通过悬停 tips 查看。
fn level_display(level: &Level, speed_only: bool) -> (String, LevelTone) {
    match level {
        Level::Running => ("⏳".to_string(), LevelTone::Pending),
        Level::Skipped => ("—".to_string(), LevelTone::Skipped),
        Level::Passed(detail) => {
            let unit = if speed_only { "/s" } else { "ms" };
            let text = detail
                .split(" · ")
                .find(|part| part.contains(unit))
                .map(compact)
                .unwrap_or_else(|| compact(detail));
            (text, LevelTone::Passed)
        }
        Level::Failed(error) => (error.to_string(), LevelTone::Failed),
    }
}

/// 去掉指标中的空白，如 "125 KB/s" -> "125KB/s"。
fn compact(text: &str) -> String {
    text.split_whitespace().collect()
}

/// 规范化 Registry URL：缺少协议时补全为 sparse+https 并追加结尾斜杠。
fn normalize_registry_url(url: &str) -> String {
    let url = url.trim();
    if url.starts_with("sparse+") || url.starts_with("http") {
        url.to_string()
    } else {
        format!("sparse+https://{url}/")
    }
}

/// 补全导出文件的 `.srmgui` 扩展名。
fn with_sources_extension(path: &Path) -> PathBuf {
    if path.extension().is_some() {
        path.to_path_buf()
    } else {
        path.with_extension(config_io::SOURCES_EXTENSION)
    }
}

/// 用系统文件管理器打开指定目录（跨平台）。
fn open_in_file_manager(path: &Path) {
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer.exe").arg(path).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(path).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}

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
fn scrolling_text(
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
fn logo_icon(size: f32) -> AnyElement {
    let image = Image::from_bytes(
        ImageFormat::Png,
        include_bytes!("../assets/logo.png").to_vec(),
    );
    img(std::sync::Arc::new(image))
        .w(px(size))
        .h(px(size))
        .flex_shrink_0()
        .into_any_element()
}

/// 「关于」模态窗口：展示应用名称、版本、说明与项目信息。
fn open_about_dialog(window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, |dialog, _window, cx| {
        let muted = cx.theme().muted_foreground;
        dialog
            .title("关于")
            .w(px(420.))
            .child(
                v_flex()
                    .w_full()
                    .items_center()
                    .gap_3()
                    .pt_2()
                    .child(logo_icon(56.))
                    .child(Label::new("Cargo 镜像源切换工具").text_lg().font_semibold())
                    .child(
                        Label::new(format!("版本 {}", env!("CARGO_PKG_VERSION")))
                            .text_sm()
                            .text_color(muted),
                    )
                    .child(
                        Label::new("用于切换 Rust Cargo 镜像源的桌面小工具")
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
                            .child(about_row("作者", env!("CARGO_PKG_AUTHORS"), muted))
                            .child(about_row("许可证", env!("CARGO_PKG_LICENSE"), muted))
                            .child(
                                h_flex()
                                    .w_full()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        Label::new("项目主页")
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
                        .label("确定")
                        .primary()
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                ),
            )
    });
}

/// 「关于」窗口中的信息行。
fn about_row(label: &'static str, value: &'static str, muted: Hsla) -> AnyElement {
    h_flex()
        .w_full()
        .items_center()
        .gap_2()
        .child(Label::new(label).text_sm().text_color(muted).w(px(64.)))
        .child(div().flex_1().text_sm().child(value))
        .into_any_element()
}

/// 贯通列表项上下的竖直分隔线（负外边距延伸进行内边距）。
fn v_separator(color: Hsla) -> AnyElement {
    div()
        .w(px(1.))
        .self_stretch()
        .mt(-px(12.))
        .mb(-px(12.))
        .bg(color)
        .into_any_element()
}

/// 步骤语义状态，用于决定文字颜色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LevelTone {
    Pending,
    Passed,
    Failed,
    Skipped,
}

/// 步骤悬停提示：说明该步骤的含义与当前状态。
fn level_tip(title: &str, level: &Level) -> String {
    match level {
        Level::Running => format!("{title}：测速中…"),
        Level::Skipped => format!("{title}：已跳过（前序步骤未通过）"),
        Level::Passed(detail) => format!("{title}：通过 · {detail}"),
        Level::Failed(error) => format!("{title}：不通过 · {error}"),
    }
}

/// 主题切换方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThemeChoice {
    System,
    Light,
    Dark,
}

/// 底部共享面板的当前内容：添加表单 / 日志面板 / 无。
///
/// 添加按钮与日志按钮共享同一块底部布局，遵循「先来后到」互斥规则：
/// 点击任一按钮，若对应面板已展开则收起，否则展开该面板并收起另一个。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BottomPanel {
    None,
    AddForm,
    LogPanel,
}

impl ThemeChoice {
    fn from_index(index: usize) -> Self {
        match index {
            1 => Self::Light,
            2 => Self::Dark,
            _ => Self::System,
        }
    }

    fn index(self) -> usize {
        match self {
            Self::System => 0,
            Self::Light => 1,
            Self::Dark => 2,
        }
    }
}

/// 应用主题：跟随系统时按当前系统外观解析，并同步原生窗口外观。
fn apply_theme(choice: ThemeChoice, cx: &mut App) {
    let appearance = match choice {
        ThemeChoice::System => None,
        ThemeChoice::Light => Some(WindowAppearance::Light),
        ThemeChoice::Dark => Some(WindowAppearance::Dark),
    };
    // 先解除/设置原生外观覆盖，再解析「跟随系统」的实际颜色。
    cx.set_window_appearance(appearance);
    let mode = match choice {
        ThemeChoice::System => ThemeMode::from(cx.window_appearance()),
        ThemeChoice::Light => ThemeMode::Light,
        ThemeChoice::Dark => ThemeMode::Dark,
    };
    Theme::change(mode, None, cx);
}

/// Source of truth for the whole UI.
#[derive(Debug, Clone)]
struct State {
    selected: SharedString,
    current: Option<SharedString>,
    active_tab: usize,
    customs: HashMap<String, CustomSource>,
    status: Status,
    probes: HashMap<String, Probe>,
    theme: ThemeChoice,
}

impl State {
    fn new(current: Option<String>, customs: HashMap<String, CustomSource>) -> Self {
        let selected = current.clone().unwrap_or_else(|| "rsproxy".to_string());
        Self {
            selected: selected.into(),
            current: current.map(Into::into),
            active_tab: 0,
            customs,
            status: Status::Info("选择一个源，然后点击「应用」写入 ~/.cargo/config.toml".into()),
            probes: HashMap::new(),
            theme: ThemeChoice::System,
        }
    }
}

/// Top-level application model.
struct CargoMirrorApp {
    state: State,
}

impl CargoMirrorApp {
    fn new(cx: &mut App) -> Entity<Self> {
        let current = config_io::current_source().ok().flatten();
        let customs = config_io::read_customs().unwrap_or_default();
        cx.new(|_cx| Self {
            state: State::new(current, customs),
        })
    }
}

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            let app = CargoMirrorApp::new(cx);
            let _ = gpui_kit::open_window(
                WindowOptions {
                    titlebar: Some(gpui_kit::TitlebarOptions {
                        title: Some("Cargo 镜像源切换工具".into()),
                        ..TitleBar::title_bar_options()
                    }),
                    app_owns_titlebar_drag: true,
                    window_bounds: Some(gpui_kit::WindowBounds::Windowed(gpui_kit::Bounds {
                        origin: gpui_kit::point(gpui_kit::px(200.), gpui_kit::px(200.)),
                        size: gpui_kit::size(gpui_kit::px(760.), gpui_kit::px(560.)),
                    })),
                    ..Default::default()
                },
                cx,
                move |window, cx| cx.new(|cx| RootView::new(window, cx, app)),
            );
        });
}

/// The single root view, split into three panels.
struct RootView {
    app: Entity<CargoMirrorApp>,
    bottom_panel: BottomPanel,
    new_id_input: Entity<InputState>,
    new_name_input: Entity<InputState>,
    new_url_input: Entity<InputState>,
    new_desc_input: Entity<InputState>,
}

impl RootView {
    fn new(window: &mut Window, cx: &mut Context<Self>, app: Entity<CargoMirrorApp>) -> Self {
        let new_id_input = cx.new(|cx| InputState::new(window, cx).placeholder("如 my-mirror"));
        let new_name_input = cx.new(|cx| InputState::new(window, cx).placeholder("如 我的镜像源"));
        let new_url_input = cx
            .new(|cx| InputState::new(window, cx).placeholder("sparse+https://example.com/index/"));
        let new_desc_input = cx.new(|cx| InputState::new(window, cx).placeholder("可选，源的说明文字"));
        let view = Self {
            app,
            bottom_panel: BottomPanel::None,
            new_id_input,
            new_name_input,
            new_url_input,
            new_desc_input,
        };

        logger::info("app", "srmgui 启动");

        // 启动时按当前选择应用主题，并在「跟随系统」时响应系统外观变化。
        let theme = view.app.read(cx).state.theme;
        apply_theme(theme, cx);
        cx.observe_window_appearance(window, |view, _window, cx| {
            if view.app.read(cx).state.theme == ThemeChoice::System {
                apply_theme(ThemeChoice::System, cx);
            }
        })
        .detach();

        // 启动时立即测速，之后按固定周期自动重新测速。
        cx.spawn(async move |view, cx| {
            loop {
                let _ = view.update(cx, |view, cx| view.measure_all(cx));
                cx.background_executor().timer(PROBE_INTERVAL).await;
            }
        })
        .detach();

        view
    }

    fn apply(&mut self, cx: &mut Context<Self>) {
        let selected: String = self.app.read(cx).state.selected.clone().into();
        let customs = self.app.read(cx).state.customs.clone();

        logger::info("apply", format!("切换到源：{selected}"));

        // Offload config write to a thread so we never block the UI.
        let selected_for_write = selected.clone();
        thread::spawn(move || {
            let result = (|| -> Result<_> {
                let content = registry::build_config(&selected_for_write, &customs);
                config_io::write_config(&content)?;
                // 同步持久化自定义源元数据（name / description）
                let _ = config_io::write_customs(&customs);
                Ok(())
            })();
            match result {
                Ok(_) => logger::info("apply", format!("配置已写入，源 {selected_for_write} 生效")),
                Err(e) => logger::error("apply", format!("写入配置失败：{e:#}")),
            }
        });

        // Update state synchronously for snappy feedback.
        self.app.update(cx, |model, cx| {
            model.state.current = Some(selected.clone().into());
            model.state.selected = selected.clone().into();
            model.state.status = Status::Success(
                format!("已切换到 {selected}，配置已写入（备份在 config.toml.bak）").into(),
            );
            cx.notify();
        });
    }

    /// 对所有内置源与自定义源发起一轮分层测速。
    fn measure_all(&mut self, cx: &mut Context<Self>) {
        let builtins = registry::builtins();
        let builtin_ids: Vec<&str> = builtins.iter().map(|r| r.id).collect();
        let targets: Vec<(String, String)> = builtins
            .iter()
            .map(|r| (r.id.to_string(), r.url.to_string()))
            .chain(
                self.app
                    .read(cx)
                    .state
                    .customs
                    .iter()
                    .filter(|(id, _)| !builtin_ids.contains(&id.as_str()))
                    .map(|(id, src)| (id.clone(), src.url.clone())),
            )
            .collect();

        for (id, url) in targets {
            self.measure(id, url, cx);
        }
    }

    /// 对一个源执行四层探测，先置为进行中，完成后写入结果。
    fn measure(&mut self, id: String, url: String, cx: &mut Context<Self>) {
        self.app.update(cx, |model, cx| {
            model.state.probes.insert(id.clone(), Probe::running());
            cx.notify();
        });

        logger::info("probe", format!("开始测速 {id} ({url})"));

        cx.spawn(async move |view, cx| {
            let report = cx
                .background_executor()
                .spawn(async move { net::probe(&url) })
                .await;
            let probe = Probe::from_report(report);

            let summary = probe_summary(&probe);
            match &probe.download {
                Level::Failed(_) | Level::Skipped => {
                    logger::warn("probe", format!("测速完成 {id}：{summary}"));
                }
                _ => {
                    logger::info("probe", format!("测速完成 {id}：{summary}"));
                }
            }

            let _ = view.update(cx, |view, cx| {
                view.app.update(cx, |model, cx| {
                    model.state.probes.insert(id.clone(), probe);
                    cx.notify();
                });
            });
        })
        .detach();
    }

    /// 选择 srmgui 源文件并导入。
    fn import_customs(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择 srmgui 自定义源文件".into()),
        });
        cx.spawn(async move |view, cx| {
            let path = match receiver.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Ok(None)) => None,
                Ok(Err(error)) => {
                    let message = format!("打开文件选择器失败：{error}");
                    let _ = view.update(cx, |view, cx| {
                        view.app.update(cx, |model, cx| {
                            model.state.status = Status::Error(message.clone().into());
                            cx.notify();
                        });
                    });
                    None
                }
                Err(_) => None,
            };
            if let Some(path) = path {
                let _ = view.update(cx, |view, cx| view.import_customs_from(&path, cx));
            }
        })
        .detach();
    }

    /// 从文件导入自定义源：跳过内置与已存在的标识，新增项立即测速。
    fn import_customs_from(&mut self, path: &Path, cx: &mut Context<Self>) {
        logger::info("import", format!("开始导入文件：{}", path.display()));

        let imported = std::fs::read_to_string(path)
            .map_err(|error| format!("读取 {} 失败：{error}", path.display()))
            .and_then(|text| {
                config_io::import_customs(&text).map_err(|error| format!("{error:#}"))
            });

        let imported = match imported {
            Ok(imported) => imported,
            Err(message) => {
                logger::error("import", format!("导入失败：{message}"));
                self.app.update(cx, |model, cx| {
                    model.state.status = Status::Error(message.into());
                    cx.notify();
                });
                return;
            }
        };

        let builtin_ids: Vec<&str> = registry::builtins().iter().map(|r| r.id).collect();
        let mut added: Vec<(String, String)> = Vec::new();
        let mut skipped = 0usize;
        self.app.update(cx, |model, cx| {
            for (id, src) in imported {
                if builtin_ids.contains(&id.as_str()) || model.state.customs.contains_key(&id) {
                    skipped += 1;
                    continue;
                }
                let url = normalize_registry_url(&src.url);
                model.state.customs.insert(
                    id.clone(),
                    CustomSource::new(src.name, url.clone(), src.description),
                );
                added.push((id, url));
            }

            model.state.status = if added.is_empty() {
                Status::Error(format!("没有导入任何源（跳过 {skipped} 个内置或重复项）").into())
            } else if skipped == 0 {
                Status::Success(format!("已导入 {} 个自定义源", added.len()).into())
            } else {
                Status::Success(
                    format!(
                        "已导入 {} 个自定义源，跳过 {skipped} 个内置或重复项",
                        added.len()
                    )
                    .into(),
                )
            };
            cx.notify();
        });

        // 持久化导入后的自定义源元数据
        let _ = config_io::write_customs(&self.app.read(cx).state.customs);

        logger::info(
            "import",
            format!("导入完成：新增 {} 个，跳过 {} 个", added.len(), skipped),
        );

        for (name, url) in added {
            self.measure(name, url, cx);
        }
    }

    /// 将自定义源导出为 srmgui 格式文件。
    fn export_customs(&mut self, cx: &mut Context<Self>) {
        let builtin_ids: Vec<&str> = registry::builtins().iter().map(|r| r.id).collect();
        let customs: HashMap<String, CustomSource> = self
            .app
            .read(cx)
            .state
            .customs
            .iter()
            .filter(|(id, _)| !builtin_ids.contains(&id.as_str()))
            .map(|(id, src)| (id.clone(), src.clone()))
            .collect();

        if customs.is_empty() {
            self.app.update(cx, |model, cx| {
                model.state.status = Status::Error("还没有可导出的自定义源".into());
                cx.notify();
            });
            return;
        }

        let directory = config_io::cargo_home()
            .ok()
            .and_then(|home| home.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."));
        let suggested = format!("srmgui-sources.{}", config_io::SOURCES_EXTENSION);
        let receiver = cx.prompt_for_new_path(&directory, Some(&suggested));
        cx.spawn(async move |view, cx| {
            let path = match receiver.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) => return,
                Ok(Err(error)) => {
                    let message = format!("打开保存对话框失败：{error}");
                    let _ = view.update(cx, |view, cx| {
                        view.app.update(cx, |model, cx| {
                            model.state.status = Status::Error(message.clone().into());
                            cx.notify();
                        });
                    });
                    return;
                }
                Err(_) => return,
            };

            let target = with_sources_extension(&path);
            let count = customs.len();
            let text = config_io::export_customs(&customs);
            let result = std::fs::write(&target, text);
            match &result {
                Ok(()) => logger::info(
                    "export",
                    format!("已导出 {count} 个自定义源到 {}", target.display()),
                ),
                Err(error) => logger::error(
                    "export",
                    format!("写入 {} 失败：{error}", target.display()),
                ),
            }
            let _ = view.update(cx, |view, cx| {
                view.app.update(cx, |model, cx| {
                    model.state.status = match result {
                        Ok(()) => Status::Success(
                            format!("已导出 {count} 个自定义源到 {}", target.display()).into(),
                        ),
                        Err(error) => {
                            Status::Error(format!("写入 {} 失败：{error}", target.display()).into())
                        }
                    };
                    cx.notify();
                });
            });
        })
        .detach();
    }

    /// 单个源的四个步骤：步骤之间用贯通上下的分隔线隔开，并各自带状态 tips。
    fn render_probe(&self, id: &str, cx: &mut Context<Self>) -> AnyElement {
        let Some(probe) = self.app.read(cx).state.probes.get(id).cloned() else {
            return div().into_any_element();
        };

        let steps = [
            ("🔌", "TCP 连通", &probe.tcp),
            ("🌐", "HTTP 索引探测", &probe.http),
            ("📄", "索引文件解析", &probe.parse),
            ("⬇️", "下载端点速度", &probe.download),
        ];

        let mut row = h_flex().items_stretch().gap_2().self_stretch();
        for (index, (icon, title, level)) in steps.iter().enumerate() {
            if index > 0 {
                row = row.child(v_separator(cx.theme().border));
            }
            let show_speed = index + 1 == steps.len();
            row = row.child(self.render_step(id, index, icon, title, level, show_speed, cx));
        }
        row.into_any_element()
    }

    /// 渲染单个步骤：固定宽度内显示图标与耗时/速度，悬停显示完整状态。
    fn render_step(
        &self,
        row_id: &str,
        index: usize,
        icon: &'static str,
        title: &'static str,
        level: &Level,
        show_speed: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (text, tone) = level_display(level, show_speed);
        let (color, bg) = match tone {
            LevelTone::Passed => (cx.theme().success, cx.theme().transparent),
            LevelTone::Failed => (cx.theme().danger, cx.theme().danger.alpha(0.18)),
            LevelTone::Pending => (cx.theme().muted_foreground, cx.theme().transparent),
            LevelTone::Skipped => (
                cx.theme().muted_foreground.alpha(0.35),
                cx.theme().transparent,
            ),
        };
        let tip = level_tip(title, level);

        v_flex()
            .w(px(STEP_WIDTH))
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .rounded_md()
            .bg(bg)
            .id(format!("probe-{row_id}-{index}"))
            .tooltip(move |_, cx| cx.new(|_| Tooltip::new(tip.clone())).into())
            .child(
                Label::new(format!("{icon} {text}"))
                    .w_full()
                    .text_sm()
                    .text_color(color)
                    .whitespace_nowrap()
                    .text_ellipsis(),
            )
            .into_any_element()
    }

    fn render_preset_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let state = &self.app.read(cx).state;
        let selected = state.selected.clone();
        let current = state.current.clone();

        v_flex()
            .id("preset-list-scroll")
            .w_full()
            .flex_1()
            .min_h_0()
            .gap_2()
            .children(registry::builtins().into_iter().map(|r| {
                let id: SharedString = r.id.into();
                let is_selected = selected == id;
                let is_current = current.as_ref().map(|c| c.as_ref()) == Some(r.id);
                let tip: SharedString = format!(
                    "标识名：{}\n名称：{}\n说明：{}\nRegistry URL：{}",
                    r.id, r.name, r.description, r.url
                )
                .into();

                h_flex()
                    .id(format!("preset-{}", r.id))
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
                    .when(is_selected, |f| f.cursor_pointer())
                    .child(
                        v_flex()
                            .w(px(NAME_WIDTH))
                            .flex_shrink_0()
                            .justify_center()
                            .gap_1()
                            .child(h_flex().items_center().gap_2().child(scrolling_text(
                                format!("name-{}", r.id),
                                r.name.into(),
                                tip.clone(),
                                NAME_WIDTH,
                                18.,
                                cx.theme().foreground,
                                true,
                            )))
                            .child(scrolling_text(
                                format!("desc-{}", r.id),
                                r.description.into(),
                                tip,
                                NAME_WIDTH,
                                14.,
                                cx.theme().muted_foreground,
                                false,
                            )),
                    )
                    .child(v_separator(cx.theme().border))
                    .child(self.render_probe(r.id, cx))
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
                                            Label::new("当前生效")
                                                .text_color(cx.theme().success)
                                                .text_sm(),
                                        )
                                        .child(Label::new("✅").text_xl())
                                        .into_any_element()
                                } else {
                                    Button::new(format!("apply-{}", r.id))
                                        .label("应用")
                                        .when(is_selected, |b| b.primary().large())
                                        .when(!is_selected, |b| b.secondary())
                                        .on_click(cx.listener({
                                            let apply_id = id.clone();
                                            move |view, _ev, _window, cx| {
                                                view.app.update(cx, |model, cx| {
                                                    model.state.selected = apply_id.clone();
                                                    cx.notify();
                                                });
                                                view.apply(cx);
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
                                model.state.selected = id.clone();
                                cx.notify();
                            });
                        }
                    }))
                    .into_any()
            }))
            .overflow_y_scrollbar()
            .into_any_element()
    }

    fn render_custom_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let state = &self.app.read(cx).state;
        let customs = state.customs.clone();
        let selected = state.selected.clone();
        let current = state.current.clone();

        if customs.is_empty() {
            return v_flex()
                .id("custom-list-scroll")
                .w_full()
                .flex_1()
                .min_h_0()
                .child(Label::new("还没有自定义源，在下方添加").text_color(cx.theme().muted_foreground))
                .overflow_y_scrollbar()
                .into_any_element();
        }

        let builtin_ids: Vec<&str> = registry::builtins().iter().map(|r| r.id).collect();
        v_flex()
            .id("custom-list-scroll")
            .w_full()
            .flex_1()
            .min_h_0()
            .gap_2()
            .children(
                customs
                    .into_iter()
                    .filter(|(id, _)| !builtin_ids.contains(&id.as_str()))
                    .map(|(id, src)| {
                let is_selected = selected.as_ref() == id;
                let is_current = current.as_ref().map(|c| c.as_ref()) == Some(id.as_str());
                let display_name = if src.name.is_empty() { id.clone() } else { src.name.clone() };
                let url_display = src.url.strip_prefix("sparse+").unwrap_or(&src.url).to_string();
                let desc_display = if src.description.is_empty() { url_display.clone() } else { src.description.clone() };
                let tip: SharedString = format!(
                    "标识名：{id}\n名称：{display_name}\n说明：{}\nRegistry URL：{}",
                    if src.description.is_empty() { "（无）" } else { &src.description },
                    src.url
                ).into();
                h_flex()
                    .id(format!("custom-{id}"))
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
                    .child(
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
                                Button::new(format!("del-{id}"))
                                    .label("删除")
                                    .custom(del_variant)
                                    .border_1()
                                    .border_color(cx.theme().danger)
                                    .on_click(cx.listener({
                                        let delete_id = id.clone();
                                        move |view, _ev, _window, cx| {
                                            logger::info("custom", format!("删除自定义源：{delete_id}"));
                                            view.app.update(cx, |model, cx| {
                                                model.state.customs.remove(&delete_id);
                                                if model.state.selected.as_ref() == delete_id {
                                                    model.state.selected = "rsproxy".into();
                                                }
                                                cx.notify();
                                            });
                                            // 持久化删除后的自定义源
                                            let _ = config_io::write_customs(&view.app.read(cx).state.customs);
                                        }
                                    }))
                            }),
                    )
                    .child(
                        v_flex()
                            .w(px(NAME_WIDTH))
                            .flex_shrink_0()
                            .justify_center()
                            .gap_1()
                            .child(scrolling_text(
                                format!("custom-name-{id}"),
                                display_name.into(),
                                tip.clone(),
                                NAME_WIDTH,
                                16.,
                                cx.theme().foreground,
                                true,
                            ))
                            .child(scrolling_text(
                                format!("custom-url-{id}"),
                                desc_display.into(),
                                tip,
                                NAME_WIDTH,
                                14.,
                                cx.theme().muted_foreground,
                                false,
                            )),
                    )
                    .child(v_separator(cx.theme().border))
                    .child(self.render_probe(&id, cx))
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
                                            Label::new("当前生效")
                                                .text_color(cx.theme().success)
                                                .text_sm(),
                                        )
                                        .child(Label::new("✅").text_xl())
                                        .into_any_element()
                                } else {
                                    Button::new(format!("apply-custom-{id}"))
                                        .label("应用")
                                        .when(is_selected, |b| b.primary().large())
                                        .when(!is_selected, |b| b.secondary())
                                        .on_click(cx.listener({
                                            let apply_id = id.clone();
                                            move |view, _ev, _window, cx| {
                                                view.app.update(cx, |model, cx| {
                                                    model.state.selected = apply_id.clone().into();
                                                    cx.notify();
                                                });
                                                view.apply(cx);
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
            }))
            .overflow_y_scrollbar()
            .into_any_element()
    }

    fn render_add_form(&self, cx: &mut Context<Self>) -> AnyElement {
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
                            .child(Input::new(&self.new_id_input).id("custom-id").text_color(cx.theme().foreground)),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .flex_1()
                            .child(Label::new("名称").text_sm())
                            .child(Input::new(&self.new_name_input).id("custom-name").text_color(cx.theme().foreground)),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .flex_grow(2.0)
                            .child(Label::new("Registry URL").text_sm())
                            .child(Input::new(&self.new_url_input).id("custom-url").text_color(cx.theme().foreground)),
                    ),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(Label::new("说明（可选）").text_sm())
                    .child(Input::new(&self.new_desc_input).id("custom-desc").text_color(cx.theme().foreground)),
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

    fn render_status_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let status = &self.app.read(cx).state.status;
        let (text, color) = match status {
            Status::Info(t) => (t.clone(), cx.theme().muted_foreground),
            Status::Success(t) => (t.clone(), cx.theme().success),
            Status::Error(t) => (t.clone(), cx.theme().danger),
        };
        let is_log_active = self.bottom_panel == BottomPanel::LogPanel;
        let log_btn = if is_log_active {
            Button::new("toggle-log")
                .label("收起日志")
                .primary()
        } else {
            Button::new("toggle-log")
                .label("日志")
                .secondary()
        }
        .on_click(cx.listener(|view, _ev, _window, cx| {
            view.bottom_panel = match view.bottom_panel {
                BottomPanel::LogPanel => BottomPanel::None,
                _ => BottomPanel::LogPanel,
            };
            cx.notify();
        }));

        h_flex()
            .p_3()
            .rounded_lg()
            .bg(cx.theme().muted)
            .border_1()
            .border_color(cx.theme().border)
            .w_full()
            .items_center()
            .child(Label::new(text).text_color(color).flex_1())
            .child(log_btn)
            .into_any()
    }

    /// 渲染底部滑出的日志面板，高度最多占窗口的 50%。
    fn render_log_panel(&self, viewport_height: f32, cx: &mut Context<Self>) -> AnyElement {
        let max_height = viewport_height * 0.5;
        let entries = logger::logger().entries();
        let log_dir = logger::logger().dir().display().to_string();

        let body = if entries.is_empty() {
            div()
                .child(
                    Label::new("暂无日志")
                        .text_color(cx.theme().muted_foreground)
                        .text_sm(),
                )
                .into_any_element()
        } else {
            // 最新的日志显示在最下方，自动滚动到底部由 overflow_y_scrollbar 处理。
            v_flex()
                .flex_1()
                .min_h_0()
                .overflow_y_scrollbar()
                .p_2()
                .gap_1()
                .children(entries.iter().map(|entry| {
                    let color = match entry.level {
                        LogLevel::Error => cx.theme().danger,
                        LogLevel::Warn => cx.theme().warning,
                        LogLevel::Info => cx.theme().foreground,
                        LogLevel::Debug => cx.theme().muted_foreground,
                    };
                    Label::new(entry.format())
                        .text_size(px(12.))
                        .text_color(color)
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .into_any_element()
                }))
                .into_any_element()
        };

        v_flex()
            .id("log-panel")
            .w_full()
            .h(px(max_height))
            .border_1()
            .border_color(cx.theme().border)
            .rounded_lg()
            .overflow_hidden()
            .child(
                h_flex()
                    .p_2()
                    .gap_2()
                    .items_center()
                    .bg(cx.theme().muted)
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(Label::new("日志").text_color(cx.theme().foreground).font_semibold())
                    .child({
                        let log_dir_for_click = log_dir.clone();
                        div()
                            .id("log-dir-link")
                            .cursor_pointer()
                            .hover(|style| style.underline())
                            .tooltip(move |_, cx| {
                                cx.new(|_| {
                                    Tooltip::new("点击在文件管理器中打开")
                                })
                                .into()
                            })
                            .on_click(cx.listener(move |_view, _ev, _window, _cx| {
                                open_in_file_manager(Path::new(&log_dir_for_click));
                            }))
                            .child(
                                Label::new(format!("日志目录：{log_dir}"))
                                    .text_color(cx.theme().muted_foreground)
                                    .text_sm(),
                            )
                    }),
            )
            .child(body)
            .into_any_element()
    }
}

impl Render for RootView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.app.read(cx).state.theme;
        let active_tab = self.app.read(cx).state.active_tab;
        let theme_app = self.app.clone();
        let viewport_height = _window.viewport_size().height.as_f32();

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .child(
                // 自绘标题栏，跟随主题配色
                TitleBar::new().child(
                    h_flex()
                        .items_center()
                        .gap_2()
                        .child(logo_icon(18.))
                        .child(Label::new("Cargo 镜像源切换工具").text_sm().font_semibold()),
                ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .p_4()
                    .gap_4()
                    .child(
                        // Header: 关于与主题切换
                        h_flex()
                            .items_center()
                            .gap_2()
                            .child(
                                Button::new("about-button")
                                    .icon(IconName::Info)
                                    .tooltip("关于")
                                    .small()
                                    .secondary()
                                    .on_click(|_, window, cx| open_about_dialog(window, cx)),
                            )
                            .child(
                                div().ml_auto().child(
                                    TabBar::new("theme-tabs")
                                        .segmented()
                                        .selected_index(theme.index())
                                        .on_click(move |index, _window, cx| {
                                            let choice = ThemeChoice::from_index(*index);
                                            theme_app.update(cx, |model, cx| {
                                                model.state.theme = choice;
                                                cx.notify();
                                            });
                                            apply_theme(choice, cx);
                                        })
                                        .child(Tab::new().label("跟随系统"))
                                        .child(Tab::new().label("浅色"))
                                        .child(Tab::new().label("深色")),
                                ),
                            ),
                    )
                // Main: independent preset and custom source tabs
                .child(
                    v_flex()
                        .flex_1()
                        .min_h_0()
                        .gap_3()
                        .child(
                            TabBar::new("registry-tabs")
                                .w_full()
                                .segmented()
                                .selected_index(active_tab)
                                .on_click(cx.listener(|view, index: &usize, _window, cx| {
                                    view.app.update(cx, |model, cx| {
                                        model.state.active_tab = *index;
                                        cx.notify();
                                    });
                                    // 切换到内置源时收起添加页面
                                    if *index == 0 && view.bottom_panel == BottomPanel::AddForm {
                                        view.bottom_panel = BottomPanel::None;
                                    }
                                    cx.notify();
                                }))
                                .child(Tab::new().flex_1().label("内置源"))
                                .child(Tab::new().flex_1().label("自定义源")),
                        )
                        .child(if active_tab == 0 {
                            self.render_preset_list(cx)
                        } else {
                            v_flex()
                                .flex_1()
                                .min_h_0()
                                .gap_3()
                                .child(
                                    h_flex()
                                        .items_center()
                                        .justify_end()
                                        .gap_2()
                                        .child({
                                            let is_add_active = self.bottom_panel == BottomPanel::AddForm;
                                            let add_btn = Button::new("toggle-add-form")
                                                .label(if is_add_active {
                                                    "收起添加"
                                                } else {
                                                    "添加"
                                                });
                                            if is_add_active {
                                                add_btn.primary()
                                            } else {
                                                add_btn.secondary()
                                            }
                                            .on_click(cx.listener(|view, _ev, _window, cx| {
                                                view.bottom_panel = match view.bottom_panel {
                                                    BottomPanel::AddForm => BottomPanel::None,
                                                    _ => BottomPanel::AddForm,
                                                };
                                                cx.notify();
                                            }))
                                        })
                                        .child(
                                            Button::new("import-customs")
                                                .label("导入")
                                                .secondary()
                                                .on_click(cx.listener(|view, _ev, _window, cx| {
                                                    view.import_customs(cx);
                                                })),
                                        )
                                        .child(
                                            Button::new("export-customs")
                                                .label("导出")
                                                .secondary()
                                                .on_click(cx.listener(|view, _ev, _window, cx| {
                                                    view.export_customs(cx);
                                                })),
                                        ),
                                )
                                .child(self.render_custom_list(cx))
                                .into_any_element()
                        }),
                )
                .children(match self.bottom_panel {
                    BottomPanel::None => vec![],
                    BottomPanel::AddForm => vec![div()
                        .id("bottom-panel-slide")
                        .with_animation(
                            "bottom-panel-slide-in",
                            Animation::new(Duration::from_millis(220)),
                            |this, delta| {
                                this.opacity(delta)
                                    .mt(px(16.0 * (1.0 - delta)))
                            },
                        )
                        .child(self.render_add_form(cx))
                        .into_any_element()],
                    BottomPanel::LogPanel => vec![div()
                        .id("bottom-panel-slide")
                        .with_animation(
                            "bottom-panel-slide-in",
                            Animation::new(Duration::from_millis(220)),
                            |this, delta| {
                                this.opacity(delta)
                                    .mt(px(16.0 * (1.0 - delta)))
                            },
                        )
                        .child(self.render_log_panel(viewport_height, cx))
                        .into_any_element()],
                })
                .child(self.render_status_bar(cx)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_choice_index_roundtrip() {
        for (index, choice) in [
            (0, ThemeChoice::System),
            (1, ThemeChoice::Light),
            (2, ThemeChoice::Dark),
        ] {
            assert_eq!(ThemeChoice::from_index(index), choice);
            assert_eq!(choice.index(), index);
        }
    }

    fn probe(tcp: Level, http: Level, parse: Level, download: Level) -> Probe {
        Probe {
            tcp,
            http,
            parse,
            download,
        }
    }

    #[test]
    fn passed_step_shows_its_duration() {
        let (text, tone) = level_display(&Level::Passed("12 ms".into()), false);
        assert_eq!(text, "12ms");
        assert_eq!(tone, LevelTone::Passed);
    }

    #[test]
    fn passed_step_drops_version_and_keeps_duration() {
        let (text, _) = level_display(&Level::Passed("1.0.229 · 285 ms".into()), false);
        assert_eq!(text, "285ms");
    }

    #[test]
    fn download_step_shows_speed_only() {
        let (text, _) = level_display(&Level::Passed("651 ms · 125 KB/s".into()), true);
        assert_eq!(text, "125KB/s");
    }

    #[test]
    fn failed_step_keeps_full_error_for_tooltip() {
        let long: SharedString = "x".repeat(30).into();
        let (text, tone) = level_display(&Level::Failed(long.clone()), false);
        assert_eq!(text, long);
        assert_eq!(tone, LevelTone::Failed);
    }

    #[test]
    fn skipped_step_has_no_detail() {
        let (text, tone) = level_display(&Level::Skipped, false);
        assert_eq!(text, "—");
        assert_eq!(tone, LevelTone::Skipped);
    }

    #[test]
    fn running_step_shows_hourglass() {
        let (text, tone) = level_display(&Level::Running, false);
        assert_eq!(text, "⏳");
        assert_eq!(tone, LevelTone::Pending);
    }

    #[test]
    fn maps_net_layers_to_ui_levels() {
        assert_eq!(Level::from_layer(net::Layer::Skipped), Level::Skipped);
        assert_eq!(
            Level::from_layer(net::Layer::Passed("ok".into())),
            Level::Passed("ok".into())
        );
        assert_eq!(
            Level::from_layer(net::Layer::Failed("boom".into())),
            Level::Failed("boom".into())
        );
    }

    #[test]
    fn probe_helper_builds_all_layers() {
        let p = probe(
            Level::Passed("12 ms".into()),
            Level::Passed("30 ms".into()),
            Level::Passed("1.0.0 · 34 ms".into()),
            Level::Failed("timeout".into()),
        );
        assert_eq!(level_display(&p.tcp, false).1, LevelTone::Passed);
        assert_eq!(level_display(&p.download, true).1, LevelTone::Failed);
    }
}
