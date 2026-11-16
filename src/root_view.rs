use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use gpui_kit::PathPromptOptions;
use gpui_kit::base::Selectable;
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants};
use gpui_kit::component::input::InputState;
use gpui_kit::component::label::Label;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::component::{IconName, TitleBar};
use gpui_kit::prelude::*;
use gpui_kit::{
    Anchor, Animation, AnimationExt as _, AnyElement, ClipboardItem, Entity, SharedString, Task,
    Window, div, px, rgb,
};

use crate::add_form::{render_add_form, render_private_form};
use crate::app::CargoMirrorApp;
use crate::cache;
use crate::cache_dialog;
use crate::config_dialog;
use crate::config_io;
use crate::config_io::CustomSource;
use crate::diagnostics;
use crate::health;
use crate::log_panel::{LogLevelFilter, render_log_panel};
use crate::logger;
use crate::manifest::{self, ManifestSource};
use crate::model::{
    Language, Level, LevelTone, Probe, Status, ThemeChoice, apply_theme, level_display,
    probe_summary,
};
use crate::net;
use crate::private_list::render_private_list;
use crate::region;
use crate::registry;
use crate::settings_dialog;
use crate::source_list::{SourceItem, SourceList};
use crate::ui::{STEP_WIDTH, level_tip, logo_icon, open_about_dialog, v_separator};
use crate::update::{self, Available, UpdateState};
use crate::util::{normalize_registry_url, with_sources_extension};

/// 底部共享面板的当前内容：添加表单 / 私有 registry 表单 / 日志面板 / 无。
///
/// 添加按钮与日志按钮共享同一块底部布局，遵循「先来后到」互斥规则：
/// 点击任一按钮，若对应面板已展开则收起，否则展开该面板并收起另一个。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BottomPanel {
    None,
    AddForm,
    RegistryForm,
    LogPanel,
}

/// The single root view, split into three panels.
pub(crate) struct RootView {
    pub(crate) app: Entity<CargoMirrorApp>,
    bottom_panel: BottomPanel,
    pub(crate) new_id_input: Entity<InputState>,
    pub(crate) new_name_input: Entity<InputState>,
    pub(crate) new_url_input: Entity<InputState>,
    pub(crate) new_desc_input: Entity<InputState>,
    pub(crate) new_registry_name_input: Entity<InputState>,
    pub(crate) new_registry_index_input: Entity<InputState>,
    pub(crate) new_registry_token_input: Entity<InputState>,
    /// 日志面板的关键字搜索框。
    pub(crate) log_search_input: Entity<InputState>,
    /// 日志面板的级别过滤条件。
    pub(crate) log_level: LogLevelFilter,
    /// 测速定时任务，配置变更时丢弃并以新周期重建。
    probe_task: Option<Task<()>>,
    /// 更新检查定时任务，配置变更时丢弃并以新周期重建。
    update_task: Option<Task<()>>,
}

impl RootView {
    pub(crate) fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        app: Entity<CargoMirrorApp>,
    ) -> Self {
        let new_id_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(rust_i18n::t!("add.placeholder_id").to_string())
        });
        let new_name_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("add.placeholder_name").to_string())
        });
        let new_url_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("add.placeholder_url").to_string())
        });
        let new_desc_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("add.placeholder_description").to_string())
        });
        let new_registry_name_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("private.placeholder_name").to_string())
        });
        let new_registry_index_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("private.placeholder_index").to_string())
        });
        let new_registry_token_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("private.placeholder_token").to_string())
        });
        // 认证 token 默认以密码形式显示，避免在界面中明文暴露。
        new_registry_token_input.update(cx, |state, cx| {
            state.set_masked(true, window, cx);
        });
        let log_search_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(rust_i18n::t!("log.search_placeholder").to_string())
        });
        let mut view = Self {
            app,
            bottom_panel: BottomPanel::None,
            new_id_input,
            new_name_input,
            new_url_input,
            new_desc_input,
            new_registry_name_input,
            new_registry_index_input,
            new_registry_token_input,
            log_search_input: log_search_input.clone(),
            log_level: LogLevelFilter::all(),
            probe_task: None,
            update_task: None,
        };

        // 搜索框内容变化时重绘日志面板，实时刷新过滤结果。
        cx.observe(&log_search_input, |_view, _input, cx| cx.notify())
            .detach();

        logger::info("app", rust_i18n::t!("log.app_start").to_string());

        // 启动时按当前选择应用主题，并在「跟随系统」时响应系统外观变化。
        let theme = view.app.read(cx).state.theme;
        apply_theme(theme, cx);
        cx.observe_window_appearance(window, |view, _window, cx| {
            if view.app.read(cx).state.theme == ThemeChoice::System {
                apply_theme(ThemeChoice::System, cx);
            }
        })
        .detach();

        // 测速与更新检查各自由一个定时任务驱动，配置变更时重建任务。
        view.start_probe_timer(cx);
        view.start_update_timer(cx, true);
        // 启动时后台刷新内置源清单（失败时继续沿用缓存 / 兜底清单）。
        view.refresh_manifest(false, cx);
        // 后台识别出口 IP 所在地区，用于在主页推荐当地镜像源。
        view.detect_region(cx);

        view
    }

    /// 后台识别所在地区（国家 + 省份）：结果写入状态并持久化，离线启动时沿用上次结果。
    fn detect_region(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |view, cx| {
            let location = cx
                .background_executor()
                .spawn(async { region::detect_location() })
                .await;
            let _ = view.update(cx, |view, cx| view.finish_region(location, cx));
        })
        .detach();
    }

    /// 处理地区识别结果：成功时更新状态与设置，失败时保留上次识别结果。
    fn finish_region(&mut self, location: Option<region::Location>, cx: &mut Context<Self>) {
        let Some(location) = location else {
            logger::error("region", rust_i18n::t!("recommend.log_failed").to_string());
            return;
        };
        // 只识别到国家时保留上次的省份信息，避免接口降级导致详情丢失。
        let (previous_region, previous_province, previous_province_name) = {
            let state = &self.app.read(cx).state;
            (
                state.region.clone(),
                state.province.clone(),
                state.province_name.clone(),
            )
        };
        let same_country = previous_region.as_deref() == Some(location.country.as_str());
        let province = location
            .province
            .clone()
            .or_else(|| same_country.then(|| previous_province.clone()).flatten());
        let province_name = location.province_name.clone().or_else(|| {
            same_country
                .then(|| previous_province_name.clone())
                .flatten()
        });
        let changed = previous_region.as_deref() != Some(location.country.as_str())
            || previous_province != province
            || previous_province_name != province_name;
        if changed {
            self.app.update(cx, |model, cx| {
                model.state.region = Some(location.country.clone());
                model.state.province = province.clone();
                model.state.province_name = province_name.clone();
                cx.notify();
            });
            let mut settings = config_io::read_settings();
            settings.region = Some(location.country.clone());
            settings.province = province;
            settings.province_name = province_name;
            let _ = config_io::write_settings(&settings);
        }
        logger::info(
            "region",
            rust_i18n::t!(
                "recommend.log_detected",
                region = region::location_label(
                    &location.country,
                    location.province.as_deref(),
                    location.province_name.as_deref()
                )
                .as_str()
            )
            .to_string(),
        );
    }

    /// 启动测速定时任务：先立即测速一次，之后按配置周期重复。
    ///
    /// 任务句柄保存在 `probe_task`，配置变更时丢弃旧句柄即可取消旧周期。
    fn start_probe_timer(&mut self, cx: &mut Context<Self>) {
        let interval = Duration::from_secs(
            config_io::effective_config(&config_io::read_settings()).probe_interval_secs,
        );
        // 先取消旧任务（丢弃即取消），避免新旧周期短暂并存。
        self.probe_task.take();
        self.probe_task = Some(cx.spawn(async move |view, cx| {
            let _ = view.update(cx, |view, cx| view.measure_all(cx));
            loop {
                cx.background_executor().timer(interval).await;
                let _ = view.update(cx, |view, cx| view.measure_all(cx));
            }
        }));
    }

    /// 启动更新检查定时任务。
    ///
    /// `check_now` 为 false 时只按新周期等待，避免每次保存设置都请求一次接口。
    fn start_update_timer(&mut self, cx: &mut Context<Self>, check_now: bool) {
        let interval = Duration::from_secs(
            config_io::effective_config(&config_io::read_settings()).update_interval_secs,
        );
        self.update_task.take();
        self.update_task = Some(cx.spawn(async move |view, cx| {
            if check_now {
                let _ = view.update(cx, |view, cx| view.check_for_updates(cx));
            }
            loop {
                cx.background_executor().timer(interval).await;
                let _ = view.update(cx, |view, cx| view.check_for_updates(cx));
                // 与更新检查同周期刷新内置源清单，保持订阅内容最新。
                let _ = view.update(cx, |view, cx| view.refresh_manifest(false, cx));
            }
        }));
    }

    /// 设置保存成功后调用：按新配置重建定时任务，并立即刷新测速结果。
    fn on_settings_saved(&mut self, cx: &mut Context<Self>) {
        self.start_probe_timer(cx);
        self.start_update_timer(cx, false);
    }

    /// 语言切换后刷新输入框占位文字。
    fn update_placeholders(&self, window: &mut Window, cx: &mut Context<Self>) {
        let inputs = [
            (&self.new_id_input, "add.placeholder_id"),
            (&self.new_name_input, "add.placeholder_name"),
            (&self.new_url_input, "add.placeholder_url"),
            (&self.new_desc_input, "add.placeholder_description"),
            (&self.new_registry_name_input, "private.placeholder_name"),
            (&self.new_registry_index_input, "private.placeholder_index"),
            (&self.new_registry_token_input, "private.placeholder_token"),
        ];
        for (input, key) in inputs {
            let text = rust_i18n::t!(key).to_string();
            input.update(cx, |state, cx| state.set_placeholder(text, window, cx));
        }
    }

    /// 切换界面语言：更新全局 locale、持久化并把状态栏恢复到提示文案。
    fn switch_language(&mut self, language: Language, window: &mut Window, cx: &mut Context<Self>) {
        rust_i18n::set_locale(language.code());
        self.update_placeholders(window, cx);
        self.app.update(cx, |model, cx| {
            model.state.language = language;
            model.state.status = Status::Info(rust_i18n::t!("status.hint").to_string().into());
            cx.notify();
        });
        self.persist_settings(cx);
        cx.notify();
    }

    /// 把当前语言与配置作用域写回设置文件。
    fn persist_settings(&self, cx: &Context<Self>) {
        let state = &self.app.read(cx).state;
        // 读取当前已有设置，保留用户可能已自定义的其他字段
        let mut settings = config_io::read_settings();
        settings.language = Some(state.language.code().to_string());
        settings.project_root = state.project_root.clone();
        settings.project_scope = state.project_scope;
        let _ = config_io::write_settings(&settings);
    }

    /// 切换配置作用域：全局 ↔ 项目，并重新读取该作用域当前生效的源。
    fn switch_scope(&mut self, project: bool, cx: &mut Context<Self>) {
        let project_root = self.app.read(cx).state.project_root.clone();
        self.app.update(cx, |model, cx| {
            model.state.project_scope = project;
            cx.notify();
        });
        // 切到项目作用域但还没选过目录时，直接弹出目录选择。
        if project && project_root.is_none() {
            self.pick_project(cx);
        }
        self.reload_current(cx);
        self.reload_registries(cx);
        self.persist_settings(cx);
        cx.notify();
    }

    /// 重新读取当前作用域正在生效的源。
    fn reload_current(&mut self, cx: &mut Context<Self>) {
        let (project_scope, project_root) = {
            let state = &self.app.read(cx).state;
            (state.project_scope, state.project_root.clone())
        };
        let current = config_io::current_source_scoped(if project_scope {
            project_root.as_deref()
        } else {
            None
        })
        .ok()
        .flatten();
        self.app.update(cx, |model, cx| {
            model.state.current = current.map(Into::into);
            cx.notify();
        });
    }

    /// 重新读取当前作用域下的私有 registry 列表。
    pub(crate) fn reload_registries(&mut self, cx: &mut Context<Self>) {
        let (project_scope, project_root) = {
            let state = &self.app.read(cx).state;
            (state.project_scope, state.project_root.clone())
        };
        let registries = config_io::read_private_registries_scoped(if project_scope {
            project_root.as_deref()
        } else {
            None
        });
        self.app.update(cx, |model, cx| {
            model.state.registries = registries;
            cx.notify();
        });
    }

    /// 选择项目目录：向上自动识别 `Cargo.toml`，并切换到项目级配置。
    fn pick_project(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(rust_i18n::t!("dialog.project_title").to_string().into()),
        });
        cx.spawn(async move |view, cx| {
            let picked = match receiver.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Ok(None)) => None,
                Ok(Err(error)) => {
                    let message =
                        rust_i18n::t!("scope.picker_failed", error = error.to_string()).to_string();
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
            if let Some(dir) = picked {
                let _ = view.update(cx, |view, cx| view.set_project_dir(dir, cx));
            }
        })
        .detach();
    }

    /// 应用目录选择结果：定位项目根目录并持久化作用域设置。
    fn set_project_dir(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        let Some(root) = config_io::detect_project_root(&dir) else {
            let message = rust_i18n::t!("scope.error_not_found", path = dir.display().to_string())
                .to_string();
            self.app.update(cx, |model, cx| {
                model.state.status = Status::Error(message.into());
                cx.notify();
            });
            return;
        };

        let root_display = root.display().to_string();
        logger::info(
            "scope",
            rust_i18n::t!("log.project_selected", path = root_display.as_str()).to_string(),
        );

        let message =
            rust_i18n::t!("status.project_selected", path = root_display.as_str()).to_string();
        self.app.update(cx, |model, cx| {
            model.state.project_root = Some(root);
            model.state.project_scope = true;
            model.state.status = Status::Success(message.into());
            cx.notify();
        });
        self.reload_current(cx);
        self.reload_registries(cx);
        self.persist_settings(cx);
    }

    /// 应用当前选中的源：先生成合并结果并展示 diff，确认后才真正写入。
    pub(crate) fn apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected: String = self.app.read(cx).state.selected.clone().into();
        let customs = self.app.read(cx).state.customs.clone();
        let builtins = self.app.read(cx).state.builtins.clone();
        let (project_scope, project_root) = {
            let state = &self.app.read(cx).state;
            (state.project_scope, state.project_root.clone())
        };

        // 项目作用域下必须先选定项目目录。
        if project_scope && project_root.is_none() {
            let message = rust_i18n::t!("status.project_required").to_string();
            self.app.update(cx, |model, cx| {
                model.state.status = Status::Error(message.into());
                cx.notify();
            });
            return;
        }

        let target_path = if project_scope {
            project_root.as_deref().map(config_io::project_config_path)
        } else {
            // 与 write_config 的目标保持一致（旧版 `config` 优先于 `config.toml`）。
            config_io::config_path().ok()
        };
        let Some(target_path) = target_path else {
            let message =
                rust_i18n::t!("preview.failed", error = rust_i18n::t!("error.home")).to_string();
            self.app.update(cx, |model, cx| {
                model.state.status = Status::Error(message.into());
                cx.notify();
            });
            return;
        };

        // 预先生成合并后的内容：只读取现有文件，不落盘。
        let content = registry::build_config(&selected, &builtins, &customs);
        let old = std::fs::read_to_string(&target_path).unwrap_or_default();
        let new = match config_io::preview_config(&target_path, &content) {
            Ok(new) => new,
            Err(error) => {
                logger::error(
                    "apply",
                    rust_i18n::t!("log.config_write_failed", error = format!("{error:#}"))
                        .to_string(),
                );
                let message =
                    rust_i18n::t!("preview.failed", error = format!("{error:#}")).to_string();
                self.app.update(cx, |model, cx| {
                    model.state.status = Status::Error(message.into());
                    cx.notify();
                });
                return;
            }
        };

        logger::info(
            "apply",
            rust_i18n::t!(
                "log.preview",
                source = selected.as_str(),
                path = target_path.display().to_string()
            )
            .to_string(),
        );

        let plan = config_dialog::ApplyPlan {
            target: target_path,
            old,
            new,
            content,
            source: selected,
            project_scope,
        };
        let entity = cx.entity();
        config_dialog::open_preview_dialog(window, cx, entity, plan);
    }

    /// 用户在预览窗口确认后写入配置，并同步刷新界面状态。
    pub(crate) fn apply_confirmed(
        &mut self,
        plan: config_dialog::ApplyPlan,
        cx: &mut Context<Self>,
    ) {
        let customs = self.app.read(cx).state.customs.clone();
        let project_root = self.app.read(cx).state.project_root.clone();
        let selected = plan.source.clone();
        let target_for_status = plan.target.display().to_string();
        let content = plan.content;
        let project_scope = plan.project_scope;

        logger::info(
            "apply",
            rust_i18n::t!("log.switch", source = selected.as_str()).to_string(),
        );

        // Offload config write to a thread so we never block the UI.
        let selected_for_write = selected.clone();
        let selected_for_log = selected.clone();
        thread::spawn(move || {
            let result = (|| -> Result<_> {
                let path = match (&project_root, project_scope) {
                    (Some(root), true) => {
                        config_io::write_project_config(root, &content, &selected_for_write)?
                    }
                    _ => config_io::write_config(&content, &selected_for_write)?,
                };
                // 同步持久化自定义源元数据（name / description）
                let _ = config_io::write_customs(&customs);
                Ok(path)
            })();
            match result {
                Ok(path) => logger::info(
                    "apply",
                    rust_i18n::t!(
                        "log.config_written",
                        source = selected_for_write.as_str(),
                        path = path.display().to_string()
                    )
                    .to_string(),
                ),
                Err(e) => logger::error(
                    "apply",
                    rust_i18n::t!("log.config_write_failed", error = format!("{e:#}")).to_string(),
                ),
            }
        });

        // Update state synchronously for snappy feedback.
        self.app.update(cx, |model, cx| {
            model.state.current = Some(selected.clone().into());
            model.state.selected = selected.clone().into();
            model.state.status = Status::Success(
                rust_i18n::t!(
                    "status.applied",
                    source = selected_for_log.as_str(),
                    path = target_for_status.as_str()
                )
                .to_string()
                .into(),
            );
            cx.notify();
        });
    }

    /// 一键回滚：把配置快照写回原文件，并刷新当前源显示。
    pub(crate) fn restore_snapshot(
        &mut self,
        snapshot: config_io::Snapshot,
        cx: &mut Context<Self>,
    ) {
        let time = snapshot.created_at.clone();
        let target = snapshot.target.display().to_string();

        match config_io::restore_snapshot(&snapshot) {
            Ok(()) => {
                logger::info(
                    "snapshot",
                    rust_i18n::t!(
                        "log.snapshot_restored",
                        time = time.as_str(),
                        path = target.as_str()
                    )
                    .to_string(),
                );
                let message = rust_i18n::t!(
                    "status.snapshot_restored",
                    time = time.as_str(),
                    path = target.as_str()
                )
                .to_string();
                self.app.update(cx, |model, cx| {
                    model.state.status = Status::Success(message.into());
                    cx.notify();
                });
                self.reload_current(cx);
                self.reload_registries(cx);
            }
            Err(error) => {
                logger::error(
                    "snapshot",
                    rust_i18n::t!("log.config_write_failed", error = format!("{error:#}"))
                        .to_string(),
                );
                let message = rust_i18n::t!(
                    "status.snapshot_restore_failed",
                    error = format!("{error:#}")
                )
                .to_string();
                self.app.update(cx, |model, cx| {
                    model.state.status = Status::Error(message.into());
                    cx.notify();
                });
            }
        }
    }

    /// 配置健康检查：扫描当前作用域的配置文件，列出残留替换块、孤立源段与语法错误。
    fn check_health(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (project_scope, project_root) = {
            let state = &self.app.read(cx).state;
            (state.project_scope, state.project_root.clone())
        };
        if project_scope && project_root.is_none() {
            self.app.update(cx, |model, cx| {
                model.state.status =
                    Status::Error(rust_i18n::t!("status.project_required").to_string().into());
                cx.notify();
            });
            return;
        }

        let scope = if project_scope {
            config_io::SCOPE_PROJECT
        } else {
            config_io::SCOPE_GLOBAL
        };
        let target = match config_io::config_path_scoped(if project_scope {
            project_root.as_deref()
        } else {
            None
        }) {
            Ok(path) => path,
            Err(error) => {
                self.health_status(
                    Status::Error(
                        rust_i18n::t!("health.failed", error = format!("{error:#}"))
                            .to_string()
                            .into(),
                    ),
                    cx,
                );
                return;
            }
        };
        let report = match health::scan(&target, scope) {
            Ok(report) => report,
            Err(error) => {
                logger::error(
                    "health",
                    rust_i18n::t!("health.log_failed", error = format!("{error:#}")).to_string(),
                );
                self.health_status(
                    Status::Error(
                        rust_i18n::t!("health.failed", error = format!("{error:#}"))
                            .to_string()
                            .into(),
                    ),
                    cx,
                );
                return;
            }
        };

        logger::info(
            "health",
            rust_i18n::t!(
                "health.log_check",
                path = target.display().to_string(),
                count = report.issues.len().to_string()
            )
            .to_string(),
        );
        let entity = cx.entity();
        config_dialog::open_health_dialog(window, cx, entity, report);
    }

    /// 一键修复健康检查发现的问题，并刷新当前源与私有 registry 显示。
    pub(crate) fn fix_health(&mut self, report: health::HealthReport, cx: &mut Context<Self>) {
        let path = report.path.display().to_string();
        match health::fix(&report.path, report.scope) {
            Ok(summary) if summary.unfixable => {
                logger::warn(
                    "health",
                    rust_i18n::t!("health.log_unfixable", path = path.as_str()).to_string(),
                );
                self.health_status(
                    Status::Error(rust_i18n::t!("health.unfixable").to_string().into()),
                    cx,
                );
            }
            Ok(summary) if summary.fixed == 0 => {
                self.health_status(
                    Status::Info(rust_i18n::t!("health.ok").to_string().into()),
                    cx,
                );
            }
            Ok(summary) => {
                let message = if summary.restored {
                    logger::warn(
                        "health",
                        rust_i18n::t!("health.log_restore", path = path.as_str()).to_string(),
                    );
                    rust_i18n::t!("health.restored", path = path.as_str()).to_string()
                } else {
                    logger::info(
                        "health",
                        rust_i18n::t!(
                            "health.log_fix",
                            count = summary.fixed.to_string(),
                            path = path.as_str()
                        )
                        .to_string(),
                    );
                    rust_i18n::t!("health.fixed", count = summary.fixed.to_string()).to_string()
                };
                self.health_status(Status::Success(message.into()), cx);
                self.reload_current(cx);
                self.reload_registries(cx);
                if let Ok(customs) = config_io::read_customs() {
                    self.app.update(cx, |model, cx| {
                        model.state.customs = customs;
                        cx.notify();
                    });
                }
            }
            Err(error) => {
                logger::error(
                    "health",
                    rust_i18n::t!("health.log_failed", error = format!("{error:#}")).to_string(),
                );
                self.health_status(
                    Status::Error(
                        rust_i18n::t!("health.failed", error = format!("{error:#}"))
                            .to_string()
                            .into(),
                    ),
                    cx,
                );
            }
        }
    }

    /// 更新状态栏并触发重绘（健康检查的公共收尾）。
    fn health_status(&mut self, status: Status, cx: &mut Context<Self>) {
        self.app.update(cx, |model, cx| {
            model.state.status = status;
            cx.notify();
        });
    }

    /// 打开缓存管理对话框：优先复用内存或应用缓存中的统计结果，缓存有更新时才重新统计。
    fn open_cache_manager(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(
            self.app.read(cx).state.cache,
            cache::CacheState::Idle | cache::CacheState::Failed(_)
        ) {
            match cache::load_report() {
                // 缓存目录未变化，直接展示上次结果，避免每次打开都全量扫描。
                Some(report) => self.set_cache_state(cache::CacheState::Ready(report), cx),
                None => self.start_cache_scan(cx),
            }
        }
        let entity = cx.entity();
        cache_dialog::open_cache_dialog(window, cx, entity);
    }

    /// 后台统计 `CARGO_HOME` 下的 registry / git 缓存体积与无效缓存。
    pub(crate) fn start_cache_scan(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.app.read(cx).state.cache,
            cache::CacheState::Scanning | cache::CacheState::Cleaning
        ) {
            return;
        }
        self.set_cache_state(cache::CacheState::Scanning, cx);

        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async { cache::scan() })
                .await;
            let _ = view.update(cx, |view, cx| match result {
                Ok(report) => view.set_cache_state(cache::CacheState::Ready(report), cx),
                Err(error) => {
                    logger::error(
                        "cache",
                        rust_i18n::t!("cache.log_scan_failed", error = format!("{error:#}"))
                            .to_string(),
                    );
                    view.set_cache_state(cache::CacheState::Failed(format!("{error:#}")), cx);
                }
            });
        })
        .detach();
    }

    /// 后台清理无效缓存，完成后自动重新统计。
    pub(crate) fn clean_cache(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.app.read(cx).state.cache,
            cache::CacheState::Scanning | cache::CacheState::Cleaning
        ) {
            return;
        }
        self.set_cache_state(cache::CacheState::Cleaning, cx);

        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async { (cache::clean(), cache::scan()) })
                .await;
            let _ = view.update(cx, |view, cx| view.finish_cache_clean(result, cx));
        })
        .detach();
    }

    /// 处理清理结果：更新状态栏，并用重新统计的结果刷新对话框。
    fn finish_cache_clean(
        &mut self,
        result: (Result<cache::CleanReport>, Result<cache::CacheReport>),
        cx: &mut Context<Self>,
    ) {
        let (cleaned, rescanned) = result;
        let status = match cleaned {
            Ok(report) if report.failed > 0 => {
                let message = rust_i18n::t!(
                    "cache.cleaned_partial",
                    count = report.removed.to_string(),
                    failed = report.failed.to_string()
                )
                .to_string();
                logger::warn("cache", message.clone());
                Status::Error(message.into())
            }
            Ok(report) if report.removed == 0 => {
                Status::Info(rust_i18n::t!("cache.invalid_none").to_string().into())
            }
            Ok(report) => {
                let size = cache::format_size(report.bytes);
                let message = rust_i18n::t!(
                    "cache.cleaned",
                    count = report.removed.to_string(),
                    size = size
                )
                .to_string();
                logger::info("cache", message.clone());
                Status::Success(message.into())
            }
            Err(error) => {
                logger::error(
                    "cache",
                    rust_i18n::t!("cache.log_clean_failed", error = format!("{error:#}"))
                        .to_string(),
                );
                Status::Error(
                    rust_i18n::t!("cache.clean_failed", error = format!("{error:#}"))
                        .to_string()
                        .into(),
                )
            }
        };
        self.app.update(cx, |model, cx| {
            model.state.status = status;
            cx.notify();
        });
        match rescanned {
            Ok(report) => self.set_cache_state(cache::CacheState::Ready(report), cx),
            Err(error) => self.set_cache_state(cache::CacheState::Failed(format!("{error:#}")), cx),
        }
    }

    /// 删除指定来源的全部缓存，完成后重新统计。
    pub(crate) fn delete_cache_source(
        &mut self,
        home: PathBuf,
        source: cache::Source,
        cx: &mut Context<Self>,
    ) {
        if matches!(
            self.app.read(cx).state.cache,
            cache::CacheState::Scanning | cache::CacheState::Cleaning
        ) {
            return;
        }
        self.set_cache_state(cache::CacheState::Cleaning, cx);

        let name = source.url.clone().unwrap_or_else(|| source.name.clone());
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let deleted = cache::delete_source(&home, &source);
                    (deleted, cache::scan())
                })
                .await;
            let _ = view.update(cx, |view, cx| view.finish_cache_delete(&name, result, cx));
        })
        .detach();
    }

    /// 处理来源删除结果：更新状态栏，并用重新统计的结果刷新对话框。
    fn finish_cache_delete(
        &mut self,
        name: &str,
        result: (cache::CleanReport, Result<cache::CacheReport>),
        cx: &mut Context<Self>,
    ) {
        let (deleted, rescanned) = result;
        let status = if deleted.failed > 0 {
            let message = rust_i18n::t!(
                "cache.source_delete_partial",
                name = name.to_string(),
                failed = deleted.failed.to_string()
            )
            .to_string();
            logger::warn("cache", message.clone());
            Status::Error(message.into())
        } else {
            let message = rust_i18n::t!(
                "cache.source_deleted",
                name = name.to_string(),
                size = cache::format_size(deleted.bytes)
            )
            .to_string();
            logger::info("cache", message.clone());
            Status::Success(message.into())
        };
        self.app.update(cx, |model, cx| {
            model.state.status = status;
            cx.notify();
        });
        match rescanned {
            Ok(report) => self.set_cache_state(cache::CacheState::Ready(report), cx),
            Err(error) => self.set_cache_state(cache::CacheState::Failed(format!("{error:#}")), cx),
        }
    }

    /// 更新缓存管理状态并触发重绘；统计结果同时写入应用缓存，供下次直接读取。
    fn set_cache_state(&mut self, state: cache::CacheState, cx: &mut Context<Self>) {
        if let cache::CacheState::Ready(report) = &state {
            cache::save_report(report);
        }
        self.app.update(cx, |model, cx| {
            model.state.cache = state;
            cx.notify();
        });
    }

    /// 对所有内置源与自定义源发起一轮分层测速。
    fn measure_all(&mut self, cx: &mut Context<Self>) {
        let (builtins, customs) = {
            let state = &self.app.read(cx).state;
            (state.builtins.clone(), state.customs.clone())
        };
        let builtin_ids: Vec<&str> = builtins.iter().map(|r| r.id.as_str()).collect();
        let targets: Vec<(String, String)> = builtins
            .iter()
            .map(|r| (r.id.clone(), r.url.clone()))
            .chain(
                customs
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
    pub(crate) fn measure(&mut self, id: String, url: String, cx: &mut Context<Self>) {
        self.app.update(cx, |model, cx| {
            model.state.probes.insert(id.clone(), Probe::running());
            cx.notify();
        });

        logger::info(
            "probe",
            rust_i18n::t!("log.probe_start", id = id.as_str(), url = url.as_str()).to_string(),
        );

        cx.spawn(async move |view, cx| {
            let report = cx
                .background_executor()
                .spawn(async move { net::probe(&url) })
                .await;
            let probe = Probe::from_report(report);

            let summary = probe_summary(&probe);
            let message = rust_i18n::t!(
                "log.probe_done",
                id = id.as_str(),
                summary = summary.as_str()
            )
            .to_string();
            match &probe.download {
                Level::Failed(_) | Level::Skipped => logger::warn("probe", message),
                _ => logger::info("probe", message),
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

    /// 后台检查是否有新版本，结果同步到界面右上角的更新提示。
    fn check_for_updates(&mut self, cx: &mut Context<Self>) {
        // 下载过程中不打断当前更新流程。
        if matches!(self.app.read(cx).state.update, UpdateState::Downloading(_)) {
            return;
        }
        self.app.update(cx, |model, cx| {
            model.state.update = UpdateState::Checking;
            cx.notify();
        });

        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async { update::check() })
                .await;
            let _ = view.update(cx, |view, cx| view.finish_check(result, cx));
        })
        .detach();
    }

    /// 处理检查结果：有新版本时展示提示，失败时仅记录日志。
    fn finish_check(&mut self, result: Result<Option<Available>, String>, cx: &mut Context<Self>) {
        let current = env!("CARGO_PKG_VERSION");
        match result {
            Ok(Some(available)) => {
                logger::info(
                    "update",
                    rust_i18n::t!(
                        "update.log_found",
                        version = available.version.as_str(),
                        current = current
                    )
                    .to_string(),
                );
                self.app.update(cx, |model, cx| {
                    model.state.update = UpdateState::Available(available);
                    cx.notify();
                });
            }
            Ok(None) => {
                logger::info(
                    "update",
                    rust_i18n::t!("update.log_none", version = current).to_string(),
                );
                self.app.update(cx, |model, cx| {
                    model.state.update = UpdateState::Idle;
                    cx.notify();
                });
            }
            Err(error) => {
                logger::warn(
                    "update",
                    rust_i18n::t!("update.log_check_failed", error = error.as_str()).to_string(),
                );
                self.app.update(cx, |model, cx| {
                    model.state.update = UpdateState::Idle;
                    cx.notify();
                });
            }
        }
    }

    /// 点击右上角更新提示：让用户选择用系统下载工具还是应用内下载。
    fn open_update_options(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.app.read(cx).state.update, UpdateState::Downloading(_)) {
            return;
        }
        let Some(available) = self.app.read(cx).state.update.available().cloned() else {
            return;
        };
        let entity = cx.entity();
        config_dialog::open_update_dialog(window, cx, entity, available);
    }

    /// 用系统默认下载工具（浏览器）打开安装包下载链接。
    pub(crate) fn open_update_in_system(&mut self, url: String, cx: &mut Context<Self>) {
        logger::info(
            "update",
            rust_i18n::t!("update.log_system_open", url = url.as_str()).to_string(),
        );
        self.app.update(cx, |model, cx| {
            model.state.status =
                Status::Info(rust_i18n::t!("update.system_opened").to_string().into());
            cx.notify();
        });
        cx.open_url(url.as_str());
    }

    /// 点击右上角更新提示：下载安装包并启动安装程序。
    pub(crate) fn start_update(&mut self, cx: &mut Context<Self>) {
        let Some(available) = self.app.read(cx).state.update.available().cloned() else {
            return;
        };
        if matches!(self.app.read(cx).state.update, UpdateState::Downloading(_)) {
            return;
        }

        logger::info(
            "update",
            rust_i18n::t!(
                "update.log_download_start",
                url = available.asset_url.as_str()
            )
            .to_string(),
        );
        self.app.update(cx, |model, cx| {
            model.state.status = Status::Info(
                rust_i18n::t!("update.downloading", version = available.version.as_str())
                    .to_string()
                    .into(),
            );
            model.state.update = UpdateState::Downloading(available.clone());
            cx.notify();
        });

        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { update::download(&available) })
                .await;
            let _ = view.update(cx, |view, cx| view.finish_download(result, cx));
        })
        .detach();
    }

    /// 下载完成后启动安装程序，并退出当前实例以便覆盖安装文件。
    fn finish_download(&mut self, result: Result<PathBuf, String>, cx: &mut Context<Self>) {
        match result {
            Ok(path) => {
                logger::info(
                    "update",
                    rust_i18n::t!("update.log_install", path = path.display().to_string())
                        .to_string(),
                );
                self.app.update(cx, |model, cx| {
                    model.state.status =
                        Status::Info(rust_i18n::t!("update.install_start").to_string().into());
                    cx.notify();
                });
                match update::launch(&path) {
                    // 安装程序已启动，退出本实例，避免安装包写入文件时被占用。
                    Ok(()) => cx.quit(),
                    Err(error) => {
                        logger::error(
                            "update",
                            rust_i18n::t!("update.log_install_failed", error = error.as_str())
                                .to_string(),
                        );
                        self.app.update(cx, |model, cx| {
                            model.state.update = match &model.state.update {
                                UpdateState::Downloading(available) => {
                                    UpdateState::Available(available.clone())
                                }
                                other => other.clone(),
                            };
                            model.state.status = Status::Error(error.clone().into());
                            cx.notify();
                        });
                    }
                }
            }
            Err(error) => {
                logger::warn(
                    "update",
                    rust_i18n::t!("update.log_download_failed", error = error.as_str()).to_string(),
                );
                self.app.update(cx, |model, cx| {
                    model.state.update = match &model.state.update {
                        UpdateState::Downloading(available) => {
                            UpdateState::Available(available.clone())
                        }
                        other => other.clone(),
                    };
                    model.state.status = Status::Error(
                        rust_i18n::t!("update.download_failed", error = error.as_str())
                            .to_string()
                            .into(),
                    );
                    cx.notify();
                });
            }
        }
    }

    /// 选择 srmgui 源文件并导入。
    fn import_customs(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(rust_i18n::t!("dialog.import_title").to_string().into()),
        });
        cx.spawn(async move |view, cx| {
            let path = match receiver.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Ok(None)) => None,
                Ok(Err(error)) => {
                    let message =
                        rust_i18n::t!("status.import_picker_failed", error = error.to_string())
                            .to_string();
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
        logger::info(
            "import",
            rust_i18n::t!("log.import_start", path = path.display().to_string()).to_string(),
        );

        let imported = std::fs::read_to_string(path)
            .map_err(|error| {
                rust_i18n::t!(
                    "status.read_failed",
                    path = path.display().to_string(),
                    error = error.to_string()
                )
                .to_string()
            })
            .and_then(|text| {
                config_io::import_customs(&text).map_err(|error| {
                    rust_i18n::t!("status.import_failed", error = format!("{error:#}")).to_string()
                })
            });

        let imported = match imported {
            Ok(imported) => imported,
            Err(message) => {
                logger::error(
                    "import",
                    rust_i18n::t!("log.import_failed", error = message.as_str()).to_string(),
                );
                self.app.update(cx, |model, cx| {
                    model.state.status = Status::Error(message.into());
                    cx.notify();
                });
                return;
            }
        };

        let builtin_ids: Vec<String> = self
            .app
            .read(cx)
            .state
            .builtins
            .iter()
            .map(|r| r.id.clone())
            .collect();
        let mut added: Vec<(String, String)> = Vec::new();
        let mut skipped = 0usize;
        self.app.update(cx, |model, cx| {
            for (id, src) in imported {
                if builtin_ids.iter().any(|builtin| builtin == &id)
                    || model.state.customs.contains_key(&id)
                {
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
                Status::Error(
                    rust_i18n::t!("status.import_empty", skipped = skipped)
                        .to_string()
                        .into(),
                )
            } else if skipped == 0 {
                Status::Success(
                    rust_i18n::t!("status.imported", count = added.len())
                        .to_string()
                        .into(),
                )
            } else {
                Status::Success(
                    rust_i18n::t!(
                        "status.imported_skipped",
                        count = added.len(),
                        skipped = skipped
                    )
                    .to_string()
                    .into(),
                )
            };
            cx.notify();
        });

        // 持久化导入后的自定义源元数据
        let _ = config_io::write_customs(&self.app.read(cx).state.customs);

        logger::info(
            "import",
            rust_i18n::t!("log.import_done", added = added.len(), skipped = skipped).to_string(),
        );

        for (name, url) in added {
            self.measure(name, url, cx);
        }
    }

    /// 后台刷新内置源清单，成功后替换列表并补测新增或地址变化的源。
    fn refresh_manifest(&mut self, notify: bool, cx: &mut Context<Self>) {
        logger::info(
            "manifest",
            rust_i18n::t!("manifest.log_start", url = manifest::MANIFEST_URL).to_string(),
        );
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async { manifest::fetch_remote() })
                .await;
            let _ = view.update(cx, |view, cx| view.finish_manifest(result, notify, cx));
        })
        .detach();
    }

    /// 处理清单拉取结果：失败时保留现有列表，手动刷新时在状态栏提示。
    fn finish_manifest(
        &mut self,
        result: Result<Vec<ManifestSource>, String>,
        notify: bool,
        cx: &mut Context<Self>,
    ) {
        let sources = match result {
            Ok(sources) => sources,
            Err(error) => {
                logger::error(
                    "manifest",
                    rust_i18n::t!("manifest.log_failed", error = error.as_str()).to_string(),
                );
                if notify {
                    self.app.update(cx, |model, cx| {
                        model.state.status = Status::Error(
                            rust_i18n::t!("manifest.refresh_failed", error = error)
                                .to_string()
                                .into(),
                        );
                        cx.notify();
                    });
                }
                return;
            }
        };

        logger::info(
            "manifest",
            rust_i18n::t!("manifest.log_done", count = sources.len()).to_string(),
        );

        let previous: HashMap<String, String> = self
            .app
            .read(cx)
            .state
            .builtins
            .iter()
            .map(|source| (source.id.clone(), source.url.clone()))
            .collect();
        let changed: Vec<(String, String)> = sources
            .iter()
            .filter(|source| previous.get(&source.id) != Some(&source.url))
            .map(|source| (source.id.clone(), source.url.clone()))
            .collect();

        self.app.update(cx, |model, cx| {
            model.state.builtins = sources;
            let selected = model.state.selected.to_string();
            let still_exists = model
                .state
                .builtins
                .iter()
                .any(|source| source.id == selected)
                || model.state.customs.contains_key(&selected);
            if !still_exists {
                if let Some(first) = model.state.builtins.first().map(|source| source.id.clone()) {
                    model.state.selected = first.into();
                }
            }
            if notify {
                model.state.status = Status::Success(
                    rust_i18n::t!("manifest.refreshed", count = model.state.builtins.len())
                        .to_string()
                        .into(),
                );
            }
            cx.notify();
        });

        for (id, url) in changed {
            self.measure(id, url, cx);
        }
    }

    /// 复制当前选中源的分享链接到剪贴板。
    fn share_selected(&mut self, cx: &mut Context<Self>) {
        let (selected, builtins, customs) = {
            let state = &self.app.read(cx).state;
            (
                state.selected.to_string(),
                state.builtins.clone(),
                state.customs.clone(),
            )
        };
        let source = builtins
            .iter()
            .find(|source| source.id == selected)
            .map(|source| {
                ManifestSource::new(
                    source.id.clone(),
                    manifest::display_name(source),
                    manifest::display_description(source),
                    source.url.clone(),
                )
            })
            .or_else(|| {
                customs.get(&selected).map(|source| {
                    ManifestSource::new(
                        selected.clone(),
                        source.name.clone(),
                        source.description.clone(),
                        source.url.clone(),
                    )
                })
            });

        let Some(source) = source else {
            self.app.update(cx, |model, cx| {
                model.state.status = Status::Error(rust_i18n::t!("share.none").to_string().into());
                cx.notify();
            });
            return;
        };

        let link = manifest::share_link(&source);
        cx.write_to_clipboard(ClipboardItem::new_string(link));
        logger::info(
            "share",
            rust_i18n::t!("share.log_copy", id = source.id.as_str()).to_string(),
        );
        let name = source.name.clone();
        self.app.update(cx, |model, cx| {
            model.state.status = Status::Success(
                rust_i18n::t!("share.copied", name = name.as_str())
                    .to_string()
                    .into(),
            );
            cx.notify();
        });
    }

    /// 导入分享链接中的源：已存在则直接选中，新增项作为自定义源并立即测速。
    pub(crate) fn import_shared_sources(
        &mut self,
        sources: Vec<ManifestSource>,
        cx: &mut Context<Self>,
    ) {
        let (builtins, customs) = {
            let state = &self.app.read(cx).state;
            (state.builtins.clone(), state.customs.clone())
        };
        let mut merged = customs;
        let mut selected: Option<String> = None;
        let mut added: Vec<(String, String)> = Vec::new();
        let mut skipped = 0usize;
        for source in sources {
            let id = source.id.clone();
            if builtins.iter().any(|builtin| builtin.id == id) || merged.contains_key(&id) {
                selected = Some(id);
                skipped += 1;
                continue;
            }
            let url = normalize_registry_url(&source.url);
            merged.insert(
                id.clone(),
                CustomSource::new(source.name, url.clone(), source.description),
            );
            added.push((id, url));
        }

        if added.is_empty() {
            let status = match selected {
                Some(id) => Status::Success(
                    rust_i18n::t!("share.selected", name = id.as_str())
                        .to_string()
                        .into(),
                ),
                None => Status::Error(rust_i18n::t!("share.import_empty").to_string().into()),
            };
            self.app.update(cx, |model, cx| {
                model.state.status = status;
                cx.notify();
            });
            return;
        }

        let count = added.len();
        let first_id = added[0].0.clone();
        self.app.update(cx, |model, cx| {
            model.state.customs = merged;
            model.state.selected = first_id.into();
            model.state.status = if skipped == 0 {
                Status::Success(
                    rust_i18n::t!("share.imported", count = count)
                        .to_string()
                        .into(),
                )
            } else {
                Status::Success(
                    rust_i18n::t!("share.imported_skipped", count = count, skipped = skipped)
                        .to_string()
                        .into(),
                )
            };
            cx.notify();
        });

        let _ = config_io::write_customs(&self.app.read(cx).state.customs);
        logger::info(
            "share",
            rust_i18n::t!("share.log_import", count = count).to_string(),
        );
        for (id, url) in added {
            self.measure(id, url, cx);
        }
    }

    /// 将自定义源导出为 srmgui 格式文件。
    fn export_customs(&mut self, cx: &mut Context<Self>) {
        let customs: HashMap<String, CustomSource> = {
            let state = &self.app.read(cx).state;
            let builtin_ids: Vec<&str> = state.builtins.iter().map(|r| r.id.as_str()).collect();
            state
                .customs
                .iter()
                .filter(|(id, _)| !builtin_ids.contains(&id.as_str()))
                .map(|(id, src)| (id.clone(), src.clone()))
                .collect()
        };

        if customs.is_empty() {
            self.app.update(cx, |model, cx| {
                model.state.status =
                    Status::Error(rust_i18n::t!("status.export_empty").to_string().into());
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
                    let message =
                        rust_i18n::t!("status.export_picker_failed", error = error.to_string())
                            .to_string();
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
            let target_path = target.display().to_string();
            let count = customs.len();
            let text = config_io::export_customs(&customs);
            let result = std::fs::write(&target, text);
            match &result {
                Ok(()) => logger::info(
                    "export",
                    rust_i18n::t!("log.exported", count = count, path = target_path.as_str())
                        .to_string(),
                ),
                Err(error) => logger::error(
                    "export",
                    rust_i18n::t!(
                        "log.export_failed",
                        path = target_path.as_str(),
                        error = error.to_string()
                    )
                    .to_string(),
                ),
            }
            let _ = view.update(cx, |view, cx| {
                view.app.update(cx, |model, cx| {
                    model.state.status = match result {
                        Ok(()) => Status::Success(
                            rust_i18n::t!(
                                "status.exported",
                                count = count,
                                path = target_path.as_str()
                            )
                            .to_string()
                            .into(),
                        ),
                        Err(error) => Status::Error(
                            rust_i18n::t!(
                                "status.export_failed",
                                path = target_path.as_str(),
                                error = error.to_string()
                            )
                            .to_string()
                            .into(),
                        ),
                    };
                    cx.notify();
                });
            });
        })
        .detach();
    }

    /// 导出诊断包：日志、Cargo 配置、应用设置与环境信息打包为 zip。
    pub(crate) fn export_diagnostics(&mut self, cx: &mut Context<Self>) {
        let (project_root, context) = {
            let state = &self.app.read(cx).state;
            let region = region::location_label(
                state.region.as_deref().unwrap_or(""),
                state.province.as_deref(),
                state.province_name.as_deref(),
            );
            let theme = match state.theme {
                ThemeChoice::System => "system",
                ThemeChoice::Light => "light",
                ThemeChoice::Dark => "dark",
            };
            let context = vec![
                (
                    "scope".to_string(),
                    if state.project_scope {
                        "project"
                    } else {
                        "global"
                    }
                    .to_string(),
                ),
                (
                    "project_root".to_string(),
                    state
                        .project_root
                        .as_ref()
                        .map(|root| root.display().to_string())
                        .unwrap_or_default(),
                ),
                (
                    "current_source".to_string(),
                    state
                        .current
                        .as_ref()
                        .map(|id| id.to_string())
                        .unwrap_or_else(|| "<none>".to_string()),
                ),
                ("selected_source".to_string(), state.selected.to_string()),
                ("region".to_string(), region),
                ("language".to_string(), state.language.code().to_string()),
                ("theme".to_string(), theme.to_string()),
            ];
            (state.project_root.clone(), context)
        };

        let directory = config_io::app_dir()
            .ok()
            .and_then(|dir| dir.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."));
        let stamp: String = logger::format_timestamp()
            .chars()
            .filter(|c| c.is_ascii_digit())
            .take(14)
            .collect();
        let suggested = format!("srmgui-diagnostics-{}-{}.zip", &stamp[0..8], &stamp[8..14]);

        let receiver = cx.prompt_for_new_path(&directory, Some(&suggested));
        cx.spawn(async move |view, cx| {
            let path = match receiver.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) => return,
                Ok(Err(error)) => {
                    let message =
                        rust_i18n::t!("log.diag_picker_failed", error = error.to_string())
                            .to_string();
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
            let target = if path.extension().is_none() {
                path.with_extension("zip")
            } else {
                path
            };
            let target_text = target.display().to_string();

            // 读盘与打包放到后台执行，避免日志较多时阻塞界面。
            let result = cx
                .background_executor()
                .spawn(async move {
                    diagnostics::export_bundle(&target, project_root.as_deref(), &context)
                })
                .await;

            match &result {
                Ok(_) => logger::info(
                    "diag",
                    rust_i18n::t!("log.diag_exported", path = target_text.as_str()).to_string(),
                ),
                Err(error) => logger::error(
                    "diag",
                    rust_i18n::t!("log.diag_export_failed", error = format!("{error:#}"))
                        .to_string(),
                ),
            }
            let _ = view.update(cx, |view, cx| {
                view.app.update(cx, |model, cx| {
                    model.state.status = match result {
                        Ok(_) => Status::Success(
                            rust_i18n::t!("log.diag_exported", path = target_text.as_str())
                                .to_string()
                                .into(),
                        ),
                        Err(error) => Status::Error(
                            rust_i18n::t!("log.diag_export_failed", error = format!("{error:#}"))
                                .to_string()
                                .into(),
                        ),
                    };
                    cx.notify();
                });
            });
        })
        .detach();
    }

    /// 单个源的四个步骤：步骤之间用贯通上下的分隔线隔开，并各自带状态 tips。
    pub(crate) fn render_probe(&self, id: &str, cx: &mut Context<Self>) -> AnyElement {
        let Some(probe) = self.app.read(cx).state.probes.get(id).cloned() else {
            return div().into_any_element();
        };

        let steps = [
            ("🔌", rust_i18n::t!("probe.tcp").to_string(), &probe.tcp),
            ("🌐", rust_i18n::t!("probe.http").to_string(), &probe.http),
            ("📄", rust_i18n::t!("probe.parse").to_string(), &probe.parse),
            (
                "⬇️",
                rust_i18n::t!("probe.download").to_string(),
                &probe.download,
            ),
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
        title: &str,
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

    /// 主页地区推荐条：按出口 IP 的国家 + 省份与最近测速结果推荐当地源，可直接应用。
    fn render_recommendation(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (location_label, id, source, is_current, basis) = {
            let state = &self.app.read(cx).state;
            let region_code = state.region.clone()?;
            let province = state.province.clone();
            let province_name = state.province_name.clone();
            let id = region::recommend(
                &region_code,
                province.as_deref(),
                &state.builtins,
                &state.probes,
            )?;
            let source = state
                .builtins
                .iter()
                .find(|source| source.id == id)?
                .clone();
            let is_current = state.current.as_ref().map(|c| c.as_ref()) == Some(id.as_str());
            let basis = state.probes.get(&id).and_then(region::basis);
            let location_label =
                region::location_label(&region_code, province.as_deref(), province_name.as_deref());
            (location_label, id, source, is_current, basis)
        };

        let name = manifest::display_name(&source);
        let description = manifest::display_description(&source);
        let basis = basis.map(|(speed, latency)| {
            rust_i18n::t!(
                "recommend.basis",
                speed = speed.as_str(),
                latency = latency.as_str()
            )
            .to_string()
        });
        let detail = match (description.is_empty(), basis) {
            (true, None) => String::new(),
            (false, None) => description,
            (true, Some(basis)) => basis,
            (false, Some(basis)) => format!("{description} · {basis}"),
        };
        let tip: SharedString = if detail.is_empty() {
            format!("{location_label}\n{}", source.url).into()
        } else {
            format!("{location_label}\n{detail}\n{}", source.url).into()
        };

        let action = if is_current {
            Label::new(rust_i18n::t!("source.current").to_string())
                .text_sm()
                .text_color(cx.theme().success)
                .into_any_element()
        } else {
            let apply_id = id.clone();
            Button::new("apply-recommended")
                .label(rust_i18n::t!("recommend.apply").to_string())
                .small()
                .primary()
                .on_click(cx.listener(move |view, _ev, window, cx| {
                    view.app.update(cx, |model, cx| {
                        model.state.selected = apply_id.clone().into();
                        cx.notify();
                    });
                    view.apply(window, cx);
                }))
                .into_any_element()
        };

        Some(
            h_flex()
                .id("region-recommendation")
                .w_full()
                .items_center()
                .gap_2()
                .p_2()
                .rounded_lg()
                .bg(cx.theme().muted)
                .border_1()
                .border_color(cx.theme().border)
                .tooltip(move |_, cx| cx.new(|_| Tooltip::new(tip.clone())).into())
                .child(
                    Label::new(
                        rust_i18n::t!(
                            "recommend.title",
                            region = location_label.as_str(),
                            source = name.as_str()
                        )
                        .to_string(),
                    )
                    .text_sm()
                    .font_semibold(),
                )
                .child(
                    div().flex_1().min_w_0().overflow_hidden().child(
                        Label::new(detail)
                            .w_full()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .whitespace_nowrap()
                            .text_ellipsis(),
                    ),
                )
                .child(div().flex_shrink_0().child(action))
                .into_any_element(),
        )
    }

    fn render_preset_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let state = &self.app.read(cx).state;
        let selected = state.selected.clone();
        let current = state.current.clone();
        let builtins = state.builtins.clone();

        let items = builtins
            .iter()
            .map(|r| {
                let id = r.id.clone();
                let name = manifest::display_name(r);
                let description = manifest::display_description(r);
                let region_label = region::source_region_label(&r.region, &r.province);
                let detail = if description.is_empty() {
                    region_label.clone()
                } else {
                    format!("{region_label} · {description}")
                };
                SourceItem {
                    name: name.clone().into(),
                    detail: detail.into(),
                    tip: rust_i18n::t!(
                        "source.tip",
                        id = id.as_str(),
                        name = name.as_str(),
                        description = description.as_str(),
                        region = region_label.as_str(),
                        url = r.url.as_str()
                    )
                    .to_string()
                    .into(),
                    is_selected: selected.as_ref() == id,
                    is_current: current.as_ref().map(|c| c.as_ref()) == Some(id.as_str()),
                    name_size: 18.,
                    deletable: false,
                    id,
                }
            })
            .collect();

        SourceList::new("preset-list-scroll", "preset")
            .items(items)
            .render(self, cx)
    }

    fn render_custom_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let state = &self.app.read(cx).state;
        let customs = state.customs.clone();
        let selected = state.selected.clone();
        let current = state.current.clone();
        let builtin_ids: Vec<String> = state.builtins.iter().map(|r| r.id.clone()).collect();
        let items = customs
            .into_iter()
            .filter(|(id, _)| !builtin_ids.contains(id))
            .map(|(id, src)| {
                let is_selected = selected.as_ref() == id;
                let is_current = current.as_ref().map(|c| c.as_ref()) == Some(id.as_str());
                let display_name = if src.name.is_empty() {
                    id.clone()
                } else {
                    src.name.clone()
                };
                let url_display = src
                    .url
                    .strip_prefix("sparse+")
                    .unwrap_or(&src.url)
                    .to_string();
                let desc_display = if src.description.is_empty() {
                    url_display.clone()
                } else {
                    src.description.clone()
                };
                let desc_tip = if src.description.is_empty() {
                    rust_i18n::t!("source.none").to_string()
                } else {
                    src.description.clone()
                };
                let tip: SharedString = rust_i18n::t!(
                    "source.tip",
                    id = id.as_str(),
                    name = display_name.as_str(),
                    description = desc_tip.as_str(),
                    region = region::label("").as_str(),
                    url = src.url.as_str()
                )
                .to_string()
                .into();
                SourceItem {
                    id,
                    name: display_name.into(),
                    detail: desc_display.into(),
                    tip,
                    name_size: 16.,
                    is_selected,
                    is_current,
                    deletable: true,
                }
            })
            .collect();

        SourceList::new("custom-list-scroll", "custom")
            .empty_hint(rust_i18n::t!("source.empty").to_string())
            .items(items)
            .render(self, cx)
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
                .label(rust_i18n::t!("button.log_collapse").to_string())
                .primary()
        } else {
            Button::new("toggle-log")
                .label(rust_i18n::t!("button.log").to_string())
                .secondary()
        }
        .on_click(cx.listener(|view, _ev, _window, cx| {
            view.bottom_panel = match view.bottom_panel {
                BottomPanel::LogPanel => BottomPanel::None,
                _ => BottomPanel::LogPanel,
            };
            cx.notify();
        }));

        let tip = text.clone();
        h_flex()
            .p_3()
            .rounded_lg()
            .bg(cx.theme().muted)
            .border_1()
            .border_color(cx.theme().border)
            .w_full()
            .items_center()
            .gap_2()
            .child(
                // 状态文字单行截断，完整内容通过悬停 tips 查看
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .id("status-text")
                    .tooltip(move |_, cx| cx.new(|_| Tooltip::new(tip.clone())).into())
                    .child(
                        Label::new(text)
                            .w_full()
                            .text_color(color)
                            .whitespace_nowrap()
                            .text_ellipsis(),
                    ),
            )
            .child(div().flex_shrink_0().child(log_btn))
            .into_any()
    }
}

impl Render for RootView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.app.read(cx).state.theme;
        let language = self.app.read(cx).state.language;
        let active_tab = self.app.read(cx).state.active_tab;
        let update_state = self.app.read(cx).state.update.clone();
        let project_scope = self.app.read(cx).state.project_scope;
        let project_root = self.app.read(cx).state.project_root.clone();
        let theme_app = self.app.clone();
        let viewport_height = _window.viewport_size().height.as_f32();

        // 右上角更新提示：仅在发现新版本时出现，点击后选择下载方式。
        let update_button = match &update_state {
            UpdateState::Available(available) => {
                let version = available.version.clone();
                Some(
                    Button::new("update-available")
                        .label(
                            rust_i18n::t!("update.available", version = version.as_str())
                                .to_string(),
                        )
                        .tooltip(rust_i18n::t!("update.tip").to_string())
                        .small()
                        .primary()
                        .on_click(cx.listener(|view, _ev, window, cx| {
                            view.open_update_options(window, cx)
                        })),
                )
            }
            UpdateState::Downloading(available) => Some(
                Button::new("update-downloading")
                    .label(
                        rust_i18n::t!("update.downloading", version = available.version.as_str())
                            .to_string(),
                    )
                    .tooltip(rust_i18n::t!("update.tip").to_string())
                    .small()
                    .secondary(),
            ),
            _ => None,
        };

        let path_text: SharedString = project_root
            .as_ref()
            .map(|root| root.display().to_string())
            .unwrap_or_else(|| rust_i18n::t!("scope.none").to_string())
            .into();
        let path_tip: SharedString = match &project_root {
            Some(root) => rust_i18n::t!("scope.tip", path = root.display().to_string())
                .to_string()
                .into(),
            None => rust_i18n::t!("scope.none").to_string().into(),
        };
        let pick_label = if project_root.is_some() {
            rust_i18n::t!("scope.change").to_string()
        } else {
            rust_i18n::t!("scope.select").to_string()
        };
        let snapshot_tip = rust_i18n::t!(
            "snapshot.tip",
            count = config_io::effective_config(&config_io::read_settings())
                .snapshot_limit
                .to_string()
        )
        .to_string();
        let health_tip = rust_i18n::t!("health.tip").to_string();

        // 目标配置（全局 / 项目）选中态样式：柔和的蓝色，深浅主题下醒目但不刺眼
        let scope_selected_style = {
            let (bg, border, fg) = if cx.theme().is_dark() {
                (rgb(0x1e3a8a), rgb(0x3b82f6), rgb(0xdbeafe))
            } else {
                (rgb(0xdbeafe), rgb(0x60a5fa), rgb(0x1d4ed8))
            };
            move |button: Button| button.bg(bg).border_color(border).text_color(fg)
        };

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .child(
                // 自绘标题栏，跟随主题配色
                TitleBar::new().child(
                    h_flex().items_center().gap_2().child(logo_icon(18.)).child(
                        Label::new(rust_i18n::t!("app.title").to_string())
                            .text_sm()
                            .font_semibold(),
                    ),
                ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .p_4()
                    .gap_4()
                    .child(
                        // Header: 关于、语言与主题切换
                        h_flex()
                            .items_center()
                            .gap_2()
                            .child(
                                Button::new("about-button")
                                    .icon(IconName::Info)
                                    .tooltip(rust_i18n::t!("button.about_tip").to_string())
                                    .small()
                                    .secondary()
                                    .on_click(|_, window, cx| open_about_dialog(window, cx)),
                            )
                            .child(
                                // 配置快照：查看最近 N 次写入并一键回滚
                                Button::new("open-snapshots")
                                    .icon(IconName::Undo)
                                    .label(rust_i18n::t!("snapshot.button").to_string())
                                    .small()
                                    .secondary()
                                    .tooltip(snapshot_tip)
                                    .on_click(cx.listener(|_view, _ev, window, cx| {
                                        let entity = cx.entity();
                                        config_dialog::open_snapshots_dialog(window, cx, entity);
                                    })),
                            )
                            .child(
                                // 配置健康检查：扫描残留替换块、孤立源段与语法错误
                                Button::new("open-health")
                                    .icon(IconName::TriangleAlert)
                                    .label(rust_i18n::t!("health.button").to_string())
                                    .small()
                                    .secondary()
                                    .tooltip(health_tip)
                                    .on_click(cx.listener(|view, _ev, window, cx| {
                                        view.check_health(window, cx);
                                    })),
                            )
                            .child(
                                // 缓存管理：统计 registry / git 缓存体积并清理无效缓存
                                Button::new("open-cache")
                                    .icon(IconName::HardDrive)
                                    .label(rust_i18n::t!("cache.button").to_string())
                                    .small()
                                    .secondary()
                                    .tooltip(rust_i18n::t!("cache.button_tip").to_string())
                                    .on_click(cx.listener(|view, _ev, window, cx| {
                                        view.open_cache_manager(window, cx);
                                    })),
                            )
                            .child(
                                // 应用设置：自定义网络参数、测速间隔、日志参数等
                                Button::new("open-settings")
                                    .icon(IconName::Settings)
                                    .label(rust_i18n::t!("settings.button").to_string())
                                    .small()
                                    .secondary()
                                    .tooltip(rust_i18n::t!("settings.button_tip").to_string())
                                    .on_click({
                                        let view = cx.entity().downgrade();
                                        move |_, window, cx| {
                                            let view = view.clone();
                                            settings_dialog::open_settings_dialog(
                                                window,
                                                cx,
                                                move |_window, cx| {
                                                    let _ = view.update(cx, |view, cx| {
                                                        view.on_settings_saved(cx);
                                                    });
                                                },
                                            );
                                        }
                                    }),
                            )
                            .child(
                                h_flex()
                                    .ml_auto()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        Button::new("language-button")
                                            .label(language.native_name())
                                            .dropdown_caret(true)
                                            .small()
                                            .secondary()
                                            .tooltip(
                                                rust_i18n::t!("button.language_tip").to_string(),
                                            )
                                            .dropdown_menu_with_anchor(Anchor::TopRight, {
                                                let view = cx.entity().downgrade();
                                                move |menu, _, _| {
                                                    let mut menu = menu;
                                                    for item in Language::ALL {
                                                        let view = view.clone();
                                                        menu = menu.item(
                                                            PopupMenuItem::new(item.native_name())
                                                                .checked(item == language)
                                                                .on_click(move |_, window, cx| {
                                                                    let _ = view.update(
                                                                        cx,
                                                                        |view, cx| {
                                                                            view.switch_language(
                                                                                item, window, cx,
                                                                            )
                                                                        },
                                                                    );
                                                                }),
                                                        );
                                                    }
                                                    menu
                                                }
                                            }),
                                    )
                                    .child(
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
                                            .child(
                                                Tab::new().label(
                                                    rust_i18n::t!("theme.system").to_string(),
                                                ),
                                            )
                                            .child(
                                                Tab::new().label(
                                                    rust_i18n::t!("theme.light").to_string(),
                                                ),
                                            )
                                            .child(
                                                Tab::new()
                                                    .label(rust_i18n::t!("theme.dark").to_string()),
                                            ),
                                    )
                                    .children(update_button),
                            ),
                    )
                    // Main: independent preset and custom source tabs
                    .child(
                        v_flex()
                            .flex_1()
                            .min_h_0()
                            .gap_3()
                            .child(
                                // 配置作用域：全局 ~/.cargo/config.toml 或项目 .cargo/config.toml
                                h_flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        Label::new(rust_i18n::t!("scope.label").to_string())
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground),
                                    )
                                    .child(
                                        // 选中态用柔和的蓝色高亮，深浅主题下都醒目但不刺眼
                                        ButtonGroup::new("scope-group")
                                            .small()
                                            .child(
                                                Button::new("scope-global")
                                                    .label(
                                                        rust_i18n::t!("scope.global").to_string(),
                                                    )
                                                    .selected(!project_scope)
                                                    .when(!project_scope, |button| {
                                                        scope_selected_style(button)
                                                    }),
                                            )
                                            .child(
                                                Button::new("scope-project")
                                                    .label(
                                                        rust_i18n::t!("scope.project").to_string(),
                                                    )
                                                    .selected(project_scope)
                                                    .when(project_scope, |button| {
                                                        // 末尾按钮默认无左边框，选中时补上，避免高亮边框开口
                                                        scope_selected_style(button).border_l_1()
                                                    }),
                                            )
                                            .on_click(cx.listener(
                                                |view, clicks: &Vec<usize>, _window, cx| {
                                                    if let Some(index) = clicks.first() {
                                                        view.switch_scope(*index == 1, cx);
                                                    }
                                                },
                                            )),
                                    )
                                    .when(project_scope, |row| {
                                        let tip = path_tip.clone();
                                        row.child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .overflow_hidden()
                                                .id("project-path")
                                                .tooltip(move |_, cx| {
                                                    cx.new(|_| Tooltip::new(tip.clone())).into()
                                                })
                                                .child(
                                                    Label::new(path_text.clone())
                                                        .w_full()
                                                        .text_sm()
                                                        .text_color(cx.theme().muted_foreground)
                                                        .whitespace_nowrap()
                                                        .text_ellipsis(),
                                                ),
                                        )
                                        .child(
                                            Button::new("pick-project")
                                                .label(pick_label.clone())
                                                .small()
                                                .secondary()
                                                .on_click(cx.listener(|view, _ev, _window, cx| {
                                                    view.pick_project(cx)
                                                })),
                                        )
                                    }),
                            )
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
                                        // 切换标签页时收起不属于当前页的底部表单
                                        if *index != 1 && view.bottom_panel == BottomPanel::AddForm
                                        {
                                            view.bottom_panel = BottomPanel::None;
                                        }
                                        if *index != 2
                                            && view.bottom_panel == BottomPanel::RegistryForm
                                        {
                                            view.bottom_panel = BottomPanel::None;
                                        }
                                        cx.notify();
                                    }))
                                    .child(
                                        Tab::new()
                                            .flex_1()
                                            .label(rust_i18n::t!("tab.builtin").to_string()),
                                    )
                                    .child(
                                        Tab::new()
                                            .flex_1()
                                            .label(rust_i18n::t!("tab.custom").to_string()),
                                    )
                                    .child(
                                        Tab::new()
                                            .flex_1()
                                            .label(rust_i18n::t!("tab.private").to_string()),
                                    ),
                            )
                            .child(if active_tab == 0 {
                                v_flex()
                                    .flex_1()
                                    .min_h_0()
                                    .gap_3()
                                    .child(
                                        h_flex()
                                            .items_center()
                                            .justify_end()
                                            .gap_2()
                                            .child(
                                                Button::new("refresh-manifest")
                                                    .label(
                                                        rust_i18n::t!("share.refresh").to_string(),
                                                    )
                                                    .tooltip(
                                                        rust_i18n::t!("share.refresh_tip")
                                                            .to_string(),
                                                    )
                                                    .secondary()
                                                    .on_click(cx.listener(
                                                        |view, _ev, _window, cx| {
                                                            view.refresh_manifest(true, cx);
                                                        },
                                                    )),
                                            )
                                            .child(
                                                Button::new("share-source")
                                                    .label(
                                                        rust_i18n::t!("share.button").to_string(),
                                                    )
                                                    .secondary()
                                                    .on_click(cx.listener(
                                                        |view, _ev, _window, cx| {
                                                            view.share_selected(cx);
                                                        },
                                                    )),
                                            )
                                            .child(
                                                Button::new("import-share-link")
                                                    .label(
                                                        rust_i18n::t!("share.import_link")
                                                            .to_string(),
                                                    )
                                                    .secondary()
                                                    .on_click(cx.listener(
                                                        |_view, _ev, window, cx| {
                                                            let entity = cx.entity();
                                                            config_dialog::open_import_link_dialog(
                                                                window, cx, entity,
                                                            );
                                                        },
                                                    )),
                                            ),
                                    )
                                    .children(self.render_recommendation(cx))
                                    .child(self.render_preset_list(cx))
                                    .into_any_element()
                            } else if active_tab == 1 {
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
                                                let is_add_active =
                                                    self.bottom_panel == BottomPanel::AddForm;
                                                let add_btn = Button::new("toggle-add-form").label(
                                                    if is_add_active {
                                                        rust_i18n::t!("button.add_collapse")
                                                            .to_string()
                                                    } else {
                                                        rust_i18n::t!("button.add").to_string()
                                                    },
                                                );
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
                                                    .label(
                                                        rust_i18n::t!("button.import").to_string(),
                                                    )
                                                    .secondary()
                                                    .on_click(cx.listener(
                                                        |view, _ev, _window, cx| {
                                                            view.import_customs(cx);
                                                        },
                                                    )),
                                            )
                                            .child(
                                                Button::new("export-customs")
                                                    .label(
                                                        rust_i18n::t!("button.export").to_string(),
                                                    )
                                                    .secondary()
                                                    .on_click(cx.listener(
                                                        |view, _ev, _window, cx| {
                                                            view.export_customs(cx);
                                                        },
                                                    )),
                                            )
                                            .child(
                                                Button::new("share-custom-source")
                                                    .label(
                                                        rust_i18n::t!("share.button").to_string(),
                                                    )
                                                    .secondary()
                                                    .on_click(cx.listener(
                                                        |view, _ev, _window, cx| {
                                                            view.share_selected(cx);
                                                        },
                                                    )),
                                            )
                                            .child(
                                                Button::new("import-custom-share-link")
                                                    .label(
                                                        rust_i18n::t!("share.import_link")
                                                            .to_string(),
                                                    )
                                                    .secondary()
                                                    .on_click(cx.listener(
                                                        |_view, _ev, window, cx| {
                                                            let entity = cx.entity();
                                                            config_dialog::open_import_link_dialog(
                                                                window, cx, entity,
                                                            );
                                                        },
                                                    )),
                                            ),
                                    )
                                    .child(self.render_custom_list(cx))
                                    .into_any_element()
                            } else {
                                v_flex()
                                    .flex_1()
                                    .min_h_0()
                                    .gap_3()
                                    .child(h_flex().items_center().justify_end().gap_2().child({
                                        let is_add_active =
                                            self.bottom_panel == BottomPanel::RegistryForm;
                                        let add_btn = Button::new("toggle-registry-form").label(
                                            if is_add_active {
                                                rust_i18n::t!("button.add_collapse").to_string()
                                            } else {
                                                rust_i18n::t!("button.add").to_string()
                                            },
                                        );
                                        if is_add_active {
                                            add_btn.primary()
                                        } else {
                                            add_btn.secondary()
                                        }
                                        .on_click(
                                            cx.listener(|view, _ev, _window, cx| {
                                                view.bottom_panel = match view.bottom_panel {
                                                    BottomPanel::RegistryForm => BottomPanel::None,
                                                    _ => BottomPanel::RegistryForm,
                                                };
                                                cx.notify();
                                            }),
                                        )
                                    }))
                                    .child(render_private_list(self, cx))
                                    .into_any_element()
                            }),
                    )
                    .children(match self.bottom_panel {
                        BottomPanel::None => vec![],
                        BottomPanel::AddForm => vec![
                            div()
                                .id("bottom-panel-slide")
                                .with_animation(
                                    "bottom-panel-slide-in",
                                    Animation::new(Duration::from_millis(220)),
                                    |this, delta| this.opacity(delta).mt(px(16.0 * (1.0 - delta))),
                                )
                                .child(render_add_form(self, cx))
                                .into_any_element(),
                        ],
                        BottomPanel::RegistryForm => vec![
                            div()
                                .id("bottom-panel-slide")
                                .with_animation(
                                    "bottom-panel-slide-in",
                                    Animation::new(Duration::from_millis(220)),
                                    |this, delta| this.opacity(delta).mt(px(16.0 * (1.0 - delta))),
                                )
                                .child(render_private_form(self, cx))
                                .into_any_element(),
                        ],
                        BottomPanel::LogPanel => vec![
                            div()
                                .id("bottom-panel-slide")
                                .with_animation(
                                    "bottom-panel-slide-in",
                                    Animation::new(Duration::from_millis(220)),
                                    |this, delta| this.opacity(delta).mt(px(16.0 * (1.0 - delta))),
                                )
                                .child(render_log_panel(self, viewport_height, cx))
                                .into_any_element(),
                        ],
                    })
                    .child(self.render_status_bar(cx)),
            )
    }
}
