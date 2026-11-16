use std::collections::HashMap;
use std::path::PathBuf;

use gpui_kit::{App, AppContext as _, Entity, SharedString};

use crate::cache::CacheState;
use crate::config_io;
use crate::config_io::{CustomSource, PrivateRegistry};
use crate::manifest::{self, ManifestSource};
use crate::model::{Language, Probe, Status, ThemeChoice};
use crate::update::UpdateState;

/// Source of truth for the whole UI.
#[derive(Debug, Clone)]
pub(crate) struct State {
    /// 当前生效的内置源清单（远端订阅 → 本地缓存 → 编译期兜底）。
    pub(crate) builtins: Vec<ManifestSource>,
    pub(crate) selected: SharedString,
    pub(crate) current: Option<SharedString>,
    pub(crate) active_tab: usize,
    pub(crate) customs: HashMap<String, CustomSource>,
    /// 当前作用域下 cargo 配置中的私有 registry。
    pub(crate) registries: Vec<PrivateRegistry>,
    pub(crate) status: Status,
    pub(crate) probes: HashMap<String, Probe>,
    pub(crate) theme: ThemeChoice,
    pub(crate) language: Language,
    /// 出口 IP 所在地区（`CN` / `GLOBAL`），用于推荐本地镜像源。
    pub(crate) region: Option<String>,
    /// 出口 IP 所在省份代码（如 `ZJ`），仅中国大陆有值。
    pub(crate) province: Option<String>,
    /// 出口 IP 所在省份显示名（接口原始值，如 `浙江` / `Zhejiang`）。
    pub(crate) province_name: Option<String>,
    pub(crate) update: UpdateState,
    /// 上次选中的项目根目录（含 `Cargo.toml` 的目录）。
    pub(crate) project_root: Option<PathBuf>,
    /// 是否写入项目级配置（`<项目根>/.cargo/config.toml`）。
    pub(crate) project_scope: bool,
    /// Cargo 缓存管理（统计与清理）的当前状态。
    pub(crate) cache: CacheState,
}

impl State {
    fn new(
        current: Option<String>,
        customs: HashMap<String, CustomSource>,
        registries: Vec<PrivateRegistry>,
        language: Language,
        region: Option<String>,
        province: Option<String>,
        province_name: Option<String>,
        project_root: Option<PathBuf>,
        project_scope: bool,
    ) -> Self {
        let selected = current.clone().unwrap_or_else(|| "rsproxy".to_string());
        Self {
            builtins: manifest::current_sources(),
            selected: selected.into(),
            current: current.map(Into::into),
            active_tab: 0,
            customs,
            registries,
            status: Status::Info(rust_i18n::t!("status.hint").to_string().into()),
            probes: HashMap::new(),
            theme: ThemeChoice::System,
            language,
            region,
            province,
            province_name,
            update: UpdateState::Idle,
            project_root,
            project_scope,
            cache: CacheState::Idle,
        }
    }
}

/// Top-level application model.
pub(crate) struct CargoMirrorApp {
    pub(crate) state: State,
}

impl CargoMirrorApp {
    pub(crate) fn new(cx: &mut App) -> Entity<Self> {
        let settings = config_io::read_settings();

        // 先按持久化的语言设置初始化 locale，后续所有文案按该语言生成。
        let language = settings
            .language
            .as_deref()
            .map(Language::from_code)
            .unwrap_or_default();
        rust_i18n::set_locale(language.code());

        // 未记住项目目录时，尝试从当前工作目录向上自动识别 Cargo.toml。
        let project_root = settings
            .project_root
            .filter(|root| root.join("Cargo.toml").is_file())
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .and_then(|dir| config_io::detect_project_root(&dir))
            });
        let project_scope = settings.project_scope && project_root.is_some();
        let region = settings.region.clone();
        let province = settings.province.clone();
        let province_name = settings.province_name.clone();

        let current = config_io::current_source_scoped(if project_scope {
            project_root.as_deref()
        } else {
            None
        })
        .ok()
        .flatten();
        let customs = config_io::read_customs().unwrap_or_default();
        let registries = config_io::read_private_registries_scoped(if project_scope {
            project_root.as_deref()
        } else {
            None
        });
        cx.new(|_cx| Self {
            state: State::new(
                current,
                customs,
                registries,
                language,
                region,
                province,
                province_name,
                project_root,
                project_scope,
            ),
        })
    }
}
