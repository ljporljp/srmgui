use anyhow::{Context, Result, bail};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use toml_edit::{DocumentMut, Item, Table, TableLike};

/// 自定义源导出文件的扩展名。
pub const SOURCES_EXTENSION: &str = "srmgui";

/// 导出文件的格式标识。
const SOURCES_FORMAT: &str = "srmgui.sources";

/// 导出文件的格式版本（v2 起支持 name / description 字段）。
const SOURCES_VERSION: i64 = 2;

/// 全局配置作用域标识。
pub const SCOPE_GLOBAL: &str = "global";

/// 项目配置作用域标识。
pub const SCOPE_PROJECT: &str = "project";

/// 单个自定义源的完整信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomSource {
    /// 展示名称。
    pub name: String,
    /// Registry URL。
    pub url: String,
    /// 说明文字。
    pub description: String,
}

impl CustomSource {
    pub fn new(
        name: impl Into<String>,
        url: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            url: url.into(),
            description: description.into(),
        }
    }
}

/// 一个私有 registry（cargo 配置中的 `[registries.<name>]`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivateRegistry {
    /// registry 名称（`registry = "..."` 中引用的标识）。
    pub name: String,
    /// 索引地址（sparse+ 或 git 索引 URL）。
    pub index: String,
    /// 是否已在 credentials 中配置认证 token。
    pub has_token: bool,
}

impl PrivateRegistry {
    pub fn new(name: impl Into<String>, index: impl Into<String>, has_token: bool) -> Self {
        Self {
            name: name.into(),
            index: index.into(),
            has_token,
        }
    }
}

/// 用户主目录。
fn home_dir() -> Result<PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .with_context(|| rust_i18n::t!("error.home").to_string())
}

/// Cargo home: `$CARGO_HOME` or `~/.cargo`.
pub fn cargo_home() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("CARGO_HOME") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    Ok(home_dir()?.join(".cargo"))
}

// 测试专用的应用目录覆盖。
//
// 集成测试会读写快照与自定义源元数据，必须重定向到临时目录，
// 不能污染真实的 `<HOME>/srmgui`。按线程保存，避免并行测试互相干扰。
#[cfg(test)]
thread_local! {
    static TEST_APP_DIR: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// 仅测试使用：把应用目录重定向到临时目录，传入 `None` 恢复默认。
#[cfg(test)]
pub(crate) fn set_test_app_dir(dir: Option<PathBuf>) {
    TEST_APP_DIR.with(|cell| *cell.borrow_mut() = dir);
    invalidate_config_cache();
}

/// 应用配置目录：`<HOME>/srmgui`，与日志目录 `<HOME>/srmgui/logs` 同级。
pub fn app_dir() -> Result<PathBuf> {
    #[cfg(test)]
    if let Some(dir) = TEST_APP_DIR.with(|cell| cell.borrow().clone()) {
        return Ok(dir);
    }
    Ok(home_dir()?.join("srmgui"))
}

/// 指定目录下 cargo 实际使用的配置文件。
///
/// cargo 同时支持无扩展名的 `config` 与 `config.toml`；两者都存在时以
/// `config` 为准（见 cargo 配置文档）。读写必须落在同一个文件上，
/// 否则写入 `config.toml` 后 cargo 仍会读取旧的 `config`，配置不生效。
fn config_file_in(dir: &Path) -> PathBuf {
    let legacy = dir.join("config");
    if legacy.is_file() {
        legacy
    } else {
        dir.join("config.toml")
    }
}

/// 全局 cargo 配置文件路径（`$CARGO_HOME` 下）。
pub fn config_path() -> Result<PathBuf> {
    Ok(config_file_in(&cargo_home()?))
}

/// 当前作用域对应的 cargo 配置文件：项目作用域用项目配置，否则用全局配置。
pub fn config_path_scoped(project_root: Option<&Path>) -> Result<PathBuf> {
    match project_root {
        Some(root) => Ok(project_config_path(root)),
        None => config_path(),
    }
}

/// cargo 认证凭据文件路径（优先旧版无扩展名的 `credentials`）。
pub fn credentials_path() -> Result<PathBuf> {
    let dir = cargo_home()?;
    let legacy = dir.join("credentials");
    if legacy.is_file() {
        Ok(legacy)
    } else {
        Ok(dir.join("credentials.toml"))
    }
}

/// 自定义源元数据文件路径（存放 name / description 等 cargo config 无法承载的信息）。
pub fn customs_path() -> Result<PathBuf> {
    Ok(app_dir()?.join("srmgui-customs.toml"))
}

/// 应用设置文件路径。
pub fn settings_path() -> Result<PathBuf> {
    Ok(app_dir()?.join("srmgui-settings.toml"))
}

/// 配置快照目录：`<HOME>/srmgui/snapshots`。
pub fn snapshot_dir() -> Result<PathBuf> {
    Ok(app_dir()?.join("snapshots"))
}

/// 从配置内容解析当前生效的源：有 `replace-with` 时取它，否则为 `official`。
///
/// 快照保存的是写入前的配置，记录源时必须从内容本身推导，
/// 否则写入前的内容会配上写入后的源标签，导致错位一格。
fn source_in_config(content: &str) -> Option<String> {
    let root: toml::Table = content.parse().ok()?;
    let replaced = root
        .get("source")
        .and_then(toml::Value::as_table)
        .and_then(|sources| sources.get("crates-io"))
        .and_then(toml::Value::as_table)
        .and_then(|crates_io| crates_io.get("replace-with"))
        .and_then(toml::Value::as_str);
    Some(replaced.unwrap_or("official").to_string())
}

/// 将快照记录中的 UTC 时间（`YYYY-MM-DD HH:MM:SS.mmm`）转换为本地时间展示。
///
/// 落盘时间保持 UTC 以保证文件名与跨时区一致；界面展示需与系统时间一致，
/// 无法解析的旧格式内容按原样返回，避免展示层出错。
pub(crate) fn snapshot_local_time(created_at: &str) -> String {
    match chrono::NaiveDateTime::parse_from_str(created_at, "%Y-%m-%d %H:%M:%S%.3f") {
        Ok(utc) => utc
            .and_utc()
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
        Err(_) => created_at.to_string(),
    }
}

/// 一次配置写入前的快照，用于出问题后一键回滚。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// 创建时间（`YYYY-MM-DD HH:MM:SS.mmm`）。
    pub created_at: String,
    /// 被快照的配置文件路径。
    pub target: PathBuf,
    /// 作用域：`global` 或 `project`。
    pub scope: String,
    /// 当时生效的源标识。
    pub source: String,
    /// 快照内容（完整配置文件文本）。
    pub content: String,
}

/// 可持久化的应用设置：界面语言、配置作用域、网络与测速参数。
///
/// 所有 `Option` 字段为 `None` 时表示用户未自定义，运行时会回退到默认值。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    /// 持久化的界面语言（如 `zh-CN` / `en-US`）。
    pub language: Option<String>,
    /// 上次识别到的所在地区（`CN` / 国家代码），用于离线时展示推荐源。
    pub region: Option<String>,
    /// 上次识别到的省份代码（如 `ZJ`），仅中国大陆有值。
    pub province: Option<String>,
    /// 上次识别到的省份显示名（接口原始值，如 `浙江` / `Zhejiang`）。
    pub province_name: Option<String>,
    /// 上次选中的项目根目录（含 `Cargo.toml` 的目录）。
    pub project_root: Option<PathBuf>,
    /// 是否写入项目级配置（`<项目根>/.cargo/config.toml`）。
    pub project_scope: bool,
    /// 网络请求超时（秒）。
    pub timeout_secs: Option<u64>,
    /// 下载测速的字节上限（KB）。
    pub download_max_kb: Option<u64>,
    /// 测速用的示例 crate 名称。
    pub sample_crate: Option<String>,
    /// 最多跟随的重定向次数。
    pub max_redirects: Option<u64>,
    /// 自动测速周期（秒）。
    pub probe_interval_secs: Option<u64>,
    /// 自动检查更新周期（秒）。
    pub update_interval_secs: Option<u64>,
    /// 快照保留数量。
    pub snapshot_limit: Option<usize>,
    /// 单个日志文件最大（MB）。
    pub max_log_mb: Option<u64>,
    /// 保留的滚动日志文件数量。
    pub max_rotated_logs: Option<usize>,
    /// 内存中保留的日志条数。
    pub max_log_entries: Option<usize>,
}

/// 运行时有效配置：所有字段都有确定值。
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub timeout_secs: u64,
    pub download_max_kb: u64,
    pub sample_crate: String,
    pub max_redirects: u64,
    pub probe_interval_secs: u64,
    pub update_interval_secs: u64,
    pub snapshot_limit: usize,
    pub max_log_mb: u64,
    pub max_rotated_logs: usize,
    pub max_log_entries: usize,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            timeout_secs: 8,
            download_max_kb: 512,
            sample_crate: "serde".to_string(),
            max_redirects: 5,
            probe_interval_secs: 180,
            update_interval_secs: 6 * 60 * 60,
            snapshot_limit: 10,
            max_log_mb: 10,
            max_rotated_logs: 5,
            max_log_entries: 1000,
        }
    }
}

/// 合并 Settings 与默认值，产出运行时有效配置。
pub fn effective_config(settings: &Settings) -> AppConfig {
    let defaults = AppConfig::default();
    AppConfig {
        timeout_secs: settings
            .timeout_secs
            .unwrap_or(defaults.timeout_secs)
            .max(1),
        download_max_kb: settings
            .download_max_kb
            .unwrap_or(defaults.download_max_kb)
            .max(1),
        sample_crate: settings
            .sample_crate
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or(defaults.sample_crate),
        max_redirects: settings
            .max_redirects
            .unwrap_or(defaults.max_redirects)
            .min(20),
        probe_interval_secs: settings
            .probe_interval_secs
            .unwrap_or(defaults.probe_interval_secs)
            .max(10),
        update_interval_secs: settings
            .update_interval_secs
            .unwrap_or(defaults.update_interval_secs)
            .max(60),
        snapshot_limit: settings
            .snapshot_limit
            .unwrap_or(defaults.snapshot_limit)
            .clamp(1, 100),
        max_log_mb: settings.max_log_mb.unwrap_or(defaults.max_log_mb).max(1),
        max_rotated_logs: settings
            .max_rotated_logs
            .unwrap_or(defaults.max_rotated_logs)
            .clamp(1, 50),
        max_log_entries: settings
            .max_log_entries
            .unwrap_or(defaults.max_log_entries)
            .clamp(100, 100000),
    }
}

/// 配置缓存代号：每次写入设置后自增，使所有线程的缓存失效。
static CONFIG_GENERATION: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// 当前线程缓存的运行时配置及其对应的配置代号。
    static CONFIG_CACHE: RefCell<(u64, Option<AppConfig>)> = const { RefCell::new((u64::MAX, None)) };
}

/// 标记配置缓存失效（设置写盘成功、或测试切换应用目录后调用）。
pub(crate) fn invalidate_config_cache() {
    CONFIG_GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// 读取运行时有效配置（带进程内缓存）。
///
/// 日志写入、测速、界面渲染等高频路径都走这里，避免每帧/每行日志反复读盘解析；
/// 只有设置真正落盘后缓存才会失效。
pub fn cached_config() -> AppConfig {
    let generation = CONFIG_GENERATION.load(Ordering::Relaxed);
    CONFIG_CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        if cache.0 == generation {
            if let Some(config) = &cache.1 {
                return config.clone();
            }
        }
        let config = effective_config(&read_settings());
        *cache = (generation, Some(config.clone()));
        config
    })
}

/// 读取应用设置，文件不存在或解析失败时回退默认值。
pub fn read_settings() -> Settings {
    let Some(text) = settings_path()
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
    else {
        return Settings::default();
    };
    let Ok(root) = text.parse::<toml::Table>() else {
        return Settings::default();
    };
    // 把可能的默认值映射回 None，避免写回时产生冗余行。
    let defaults = AppConfig::default();
    let text = |key: &str| {
        root.get(key)
            .and_then(toml::Value::as_str)
            .map(str::to_string)
    };
    let int = |key: &str| root.get(key).and_then(toml::Value::as_integer);
    let opt_u64 = |key: &str, def: u64| int(key).map(|v| v as u64).filter(|v| *v != def);
    let opt_usize = |key: &str, def: usize| int(key).map(|v| v as usize).filter(|v| *v != def);
    let opt_str = |key: &str, def: &str| text(key).filter(|value| value != def);

    Settings {
        language: text("language"),
        region: text("region"),
        province: text("province"),
        province_name: text("province_name"),
        project_root: text("project_root").map(PathBuf::from),
        project_scope: text("scope").as_deref() == Some("project"),
        timeout_secs: opt_u64("timeout_secs", defaults.timeout_secs),
        download_max_kb: opt_u64("download_max_kb", defaults.download_max_kb),
        sample_crate: opt_str("sample_crate", &defaults.sample_crate),
        max_redirects: opt_u64("max_redirects", defaults.max_redirects),
        probe_interval_secs: opt_u64("probe_interval_secs", defaults.probe_interval_secs),
        update_interval_secs: opt_u64("update_interval_secs", defaults.update_interval_secs),
        snapshot_limit: opt_usize("snapshot_limit", defaults.snapshot_limit),
        max_log_mb: opt_u64("max_log_mb", defaults.max_log_mb),
        max_rotated_logs: opt_usize("max_rotated_logs", defaults.max_rotated_logs),
        max_log_entries: opt_usize("max_log_entries", defaults.max_log_entries),
    }
}

/// 持久化应用设置。只写入与默认值不同的字段，保持文件简洁。
pub fn write_settings(settings: &Settings) -> Result<()> {
    let path = settings_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            rust_i18n::t!("error.create_dir", path = parent.display().to_string()).to_string()
        })?;
    }
    let defaults = AppConfig::default();
    let mut content = String::from("# srmgui 设置（由 srmgui 管理，请勿手动编辑）\n");
    if let Some(code) = &settings.language {
        content.push_str(&format!("language = {}\n", quote(code)));
    }
    if let Some(region) = &settings.region {
        content.push_str(&format!("region = {}\n", quote(region)));
    }
    if let Some(province) = &settings.province {
        content.push_str(&format!("province = {}\n", quote(province)));
    }
    if let Some(name) = &settings.province_name {
        content.push_str(&format!("province_name = {}\n", quote(name)));
    }
    let scope = if settings.project_scope {
        "project"
    } else {
        "global"
    };
    content.push_str(&format!("scope = {}\n", quote(scope)));
    if let Some(root) = &settings.project_root {
        content.push_str(&format!(
            "project_root = {}\n",
            quote(&root.display().to_string())
        ));
    }
    // 仅写入与默认值不同的参数
    macro_rules! push_number {
        ($key:literal, $value:expr, $default:expr) => {
            if let Some(value) = $value.filter(|v| *v != $default) {
                content.push_str(&format!(concat!($key, " = {}\n"), value));
            }
        };
    }
    push_number!("timeout_secs", settings.timeout_secs, defaults.timeout_secs);
    push_number!(
        "download_max_kb",
        settings.download_max_kb,
        defaults.download_max_kb
    );
    if let Some(krate) = settings
        .sample_crate
        .as_deref()
        .filter(|v| *v != defaults.sample_crate)
    {
        content.push_str(&format!("sample_crate = {}\n", quote(krate)));
    }
    push_number!(
        "max_redirects",
        settings.max_redirects,
        defaults.max_redirects
    );
    push_number!(
        "probe_interval_secs",
        settings.probe_interval_secs,
        defaults.probe_interval_secs
    );
    push_number!(
        "update_interval_secs",
        settings.update_interval_secs,
        defaults.update_interval_secs
    );
    push_number!(
        "snapshot_limit",
        settings.snapshot_limit,
        defaults.snapshot_limit
    );
    push_number!("max_log_mb", settings.max_log_mb, defaults.max_log_mb);
    push_number!(
        "max_rotated_logs",
        settings.max_rotated_logs,
        defaults.max_rotated_logs
    );
    push_number!(
        "max_log_entries",
        settings.max_log_entries,
        defaults.max_log_entries
    );
    std::fs::write(&path, content).with_context(|| {
        rust_i18n::t!("error.write_file", path = path.display().to_string()).to_string()
    })?;
    invalidate_config_cache();
    Ok(())
}

/// 项目级 cargo 配置路径：`<项目根>/.cargo/config.toml`（存在旧版 `config` 时优先用它）。
pub fn project_config_path(root: &Path) -> PathBuf {
    config_file_in(&root.join(".cargo"))
}

/// 从给定目录向上查找最近的 `Cargo.toml`，返回它所在的目录。
///
/// 用户可能选中工作区里的子 crate，向上回溯可以定位到真正需要写
/// `.cargo/config.toml` 的项目 / 工作区根目录。
pub fn detect_project_root(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .find(|ancestor| ancestor.join("Cargo.toml").is_file())
        .map(Path::to_path_buf)
}

/// Read which source id is currently in effect in the global config.
///
/// `replace-with` 决定被替换成的源；没有该字段时 cargo 直接使用默认的
/// crates.io（即内置的 `official`），因此返回 `official` 而不是 `None`。
/// 只有配置文件不存在（从未写入过配置）时才返回 `None`。
pub fn current_source() -> Result<Option<String>> {
    current_source_at(&config_path()?)
}

/// 读取指定 cargo 配置文件中的当前生效源（规则同 [`current_source`]）。
pub fn current_source_at(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path).with_context(|| {
        rust_i18n::t!("error.read_file", path = path.display().to_string()).to_string()
    })?;
    // 先校验语法以便把错误完整上报给界面，再复用统一的内容推导规则。
    text.parse::<toml::Table>()
        .with_context(|| rust_i18n::t!("error.parse_cargo_config").to_string())?;
    Ok(source_in_config(&text))
}

/// 读取指定作用域当前生效的源。
///
/// `project_root` 为 `None` 时读取全局配置；项目级配置文件不存在时，
/// cargo 会沿用全局配置，因此这里同样回退到全局读取。
pub fn current_source_scoped(project_root: Option<&Path>) -> Result<Option<String>> {
    if let Some(root) = project_root {
        let path = project_config_path(root);
        if path.exists() {
            return current_source_at(&path);
        }
    }
    current_source()
}

/// 读取自定义源：优先从 srmgui 元数据文件读取（含 name / description），
/// 再从 cargo config.toml 补充遗漏的源（name 回退为 id，description 为空）。
pub fn read_customs() -> Result<HashMap<String, CustomSource>> {
    let mut customs = HashMap::new();

    // 1. 从 cargo config.toml 读取所有自定义源（id -> url）
    let config = config_path()
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| text.parse::<toml::Table>().ok());
    if let Some(sources) = config
        .as_ref()
        .and_then(|root| root.get("source"))
        .and_then(toml::Value::as_table)
    {
        for (id, value) in sources {
            let Some(url) = value.get("registry").and_then(toml::Value::as_str) else {
                continue;
            };
            if id != "crates-io" {
                customs.insert(id.clone(), CustomSource::new(id.clone(), url, ""));
            }
        }
    }

    // 2. 用 srmgui 元数据文件覆盖 name / description
    if let Some(meta) = customs_path()
        .ok()
        .filter(|path| path.is_file())
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| parse_customs_metadata(&text).ok())
    {
        customs.extend(meta);
    }

    Ok(customs)
}

/// 读取 srmgui 元数据文件中登记的自定义源标识。
///
/// 与 [`read_customs`] 不同，这里只读取 srmgui 自己管理的元数据，
/// 不合并 cargo 配置里出现的其他源，供健康检查区分「孤立的源段」。
pub(crate) fn custom_metadata_ids() -> HashSet<String> {
    customs_path()
        .ok()
        .filter(|path| path.is_file())
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| parse_customs_metadata(&text).ok())
        .map(|customs| customs.into_keys().collect())
        .unwrap_or_default()
}

/// 解析 srmgui 元数据文件（内部存储格式）。
fn parse_customs_metadata(text: &str) -> Result<HashMap<String, CustomSource>> {
    let root: toml::Table = text
        .parse()
        .with_context(|| rust_i18n::t!("error.parse_metadata").to_string())?;
    let Some(toml::Value::Array(customs)) = root.get("custom") else {
        return Ok(HashMap::new());
    };

    let mut out = HashMap::new();
    for (index, item) in customs.iter().enumerate() {
        let toml::Value::Table(entry) = item else {
            bail!(
                "{}",
                rust_i18n::t!("error.metadata_entry", index = index + 1)
            );
        };
        let id = entry
            .get("id")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .with_context(|| {
                rust_i18n::t!("error.metadata_missing_id", index = index + 1).to_string()
            })?;
        let url = entry
            .get("url")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .with_context(|| rust_i18n::t!("error.metadata_missing_url", id = id).to_string())?;
        let name = entry
            .get("name")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(id);
        let description = entry
            .get("description")
            .and_then(toml::Value::as_str)
            .unwrap_or("");
        out.insert(id.to_string(), CustomSource::new(name, url, description));
    }
    Ok(out)
}

/// 持久化自定义源元数据到 `<HOME>/srmgui/srmgui-customs.toml`。
pub fn write_customs(customs: &HashMap<String, CustomSource>) -> Result<()> {
    let path = customs_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            rust_i18n::t!("error.create_dir", path = parent.display().to_string()).to_string()
        })?;
    }

    let mut ids: Vec<&String> = customs.keys().collect();
    ids.sort();

    let mut out = String::from("# srmgui 自定义源元数据（由 srmgui 管理，请勿手动编辑）\n");
    out.push_str("version = 1\n");
    for id in ids {
        let src = &customs[id];
        out.push_str("\n[[custom]]\n");
        out.push_str(&format!("id = {}\n", quote(id)));
        out.push_str(&format!("name = {}\n", quote(&src.name)));
        out.push_str(&format!("url = {}\n", quote(&src.url)));
        out.push_str(&format!("description = {}\n", quote(&src.description)));
    }
    std::fs::write(&path, out).with_context(|| {
        rust_i18n::t!("error.write_file", path = path.display().to_string()).to_string()
    })?;
    Ok(())
}

/// 读取指定 cargo 配置文件中的私有 registry 列表（按名称排序）。
///
/// `[registries.<name>]` 只承载索引地址；认证 token 通常位于
/// `$CARGO_HOME/credentials.toml` 的 `[registries.<name>]` 中。为兼容旧配置，
/// 也识别直接写在 cargo 配置里的 `token` 字段。
pub fn read_private_registries(config_path: &Path) -> Result<Vec<PrivateRegistry>> {
    let text = read_optional(config_path)?;
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let root: toml::Table = text
        .parse()
        .with_context(|| rust_i18n::t!("error.parse_cargo_config").to_string())?;
    let Some(toml::Value::Table(registries)) = root.get("registries") else {
        return Ok(Vec::new());
    };

    let tokens = credential_tokens();
    let mut out = Vec::new();
    for (name, value) in registries {
        let toml::Value::Table(entry) = value else {
            continue;
        };
        let Some(index) = entry.get("index").and_then(toml::Value::as_str) else {
            continue;
        };
        let has_token =
            entry.get("token").and_then(toml::Value::as_str).is_some() || tokens.contains(name);
        out.push(PrivateRegistry::new(
            name.clone(),
            index.to_string(),
            has_token,
        ));
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// 读取当前作用域下的私有 registry 列表。
pub fn read_private_registries_scoped(project_root: Option<&Path>) -> Vec<PrivateRegistry> {
    config_path_scoped(project_root)
        .ok()
        .and_then(|path| read_private_registries(&path).ok())
        .unwrap_or_default()
}

/// 新增或更新私有 registry：写入 `[registries.<name>] index`，
/// 可选地把 token 写入 cargo credentials 文件。
pub fn upsert_private_registry(
    config_path: &Path,
    scope: &str,
    name: &str,
    index: &str,
    token: Option<&str>,
) -> Result<()> {
    let existing = read_optional(config_path)?;
    let merged = merge_registry_entry(&existing, name, index)?;
    commit_merged(config_path, scope, "", &existing, &merged)?;

    if let Some(token) = token.map(str::trim).filter(|token| !token.is_empty()) {
        set_registry_token(name, token)?;
    }
    Ok(())
}

/// 删除指定 registry，并在没有其他配置继续使用它时清理认证 token。
pub fn remove_private_registry(path: &Path, scope: &str, name: &str) -> Result<()> {
    let existing = read_optional(path)?;
    let merged = remove_registry_entry(&existing, name)?;
    commit_merged(path, scope, "", &existing, &merged)?;

    // 全局配置里仍定义同名 registry 时保留 token，避免影响其他作用域。
    let still_defined = config_defines_registry(path, name)
        || config_path()
            .ok()
            .filter(|global| global != path)
            .is_some_and(|global| config_defines_registry(&global, name));
    if !still_defined {
        let _ = remove_registry_token(name);
    }
    Ok(())
}

/// 把 token 写入 `$CARGO_HOME/credentials.toml` 的 `[registries.<name>]`。
pub fn set_registry_token(name: &str, token: &str) -> Result<PathBuf> {
    let path = credentials_path()?;
    let existing = read_optional(&path)?;
    let merged = merge_token_entry(&existing, name, token)?;
    if existing != merged {
        write_merged(&path, &merged)?;
    }
    Ok(path)
}

/// 从 credentials 文件中删除指定 registry 的 token。
pub fn remove_registry_token(name: &str) -> Result<()> {
    let path = credentials_path()?;
    let existing = read_optional(&path)?;
    let merged = remove_registry_entry(&existing, name)?;
    if existing != merged {
        write_merged(&path, &merged)?;
    }
    Ok(())
}

/// 读取 credentials 中已配置 token 的 registry 名称集合。
fn credential_tokens() -> HashSet<String> {
    let Some(path) = credentials_path().ok() else {
        return HashSet::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashSet::new();
    };
    let Ok(root) = text.parse::<toml::Table>() else {
        return HashSet::new();
    };
    let Some(toml::Value::Table(registries)) = root.get("registries") else {
        return HashSet::new();
    };
    registries
        .iter()
        .filter(|(_, value)| {
            value
                .as_table()
                .and_then(|entry| entry.get("token"))
                .and_then(toml::Value::as_str)
                .is_some()
        })
        .map(|(name, _)| name.clone())
        .collect()
}

/// 指定配置文件中是否仍定义该 registry。
fn config_defines_registry(path: &Path, name: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(root) = text.parse::<toml::Table>() else {
        return false;
    };
    root.get("registries")
        .and_then(toml::Value::as_table)
        .is_some_and(|registries| registries.contains_key(name))
}

/// 把自定义源导出为 srmgui 定义的文本格式（TOML 子集，按标识名排序）。
pub fn export_customs(customs: &HashMap<String, CustomSource>) -> String {
    let mut ids: Vec<&String> = customs.keys().collect();
    ids.sort();

    let mut out = String::from("# srmgui 自定义源文件，可通过 srmgui 导入\n");
    out.push_str(&format!("format = {}\n", quote(SOURCES_FORMAT)));
    out.push_str(&format!("version = {}\n", SOURCES_VERSION));
    for id in ids {
        let src = &customs[id];
        out.push_str("\n[[source]]\n");
        out.push_str(&format!("id = {}\n", quote(id)));
        out.push_str(&format!("name = {}\n", quote(&src.name)));
        out.push_str(&format!("url = {}\n", quote(&src.url)));
        out.push_str(&format!("description = {}\n", quote(&src.description)));
    }
    out
}

/// 解析 srmgui 自定义源文件，支持 v1（name 即 id）与 v2（id + name + description）。
pub fn import_customs(text: &str) -> Result<HashMap<String, CustomSource>> {
    let root: toml::Table = text
        .parse()
        .with_context(|| rust_i18n::t!("error.parse_sources").to_string())?;

    match root.get("format") {
        Some(toml::Value::String(format)) if format == SOURCES_FORMAT => {}
        _ => bail!(
            "{}",
            rust_i18n::t!("error.bad_format", format = SOURCES_FORMAT)
        ),
    }
    let version = match root.get("version") {
        Some(toml::Value::Integer(v)) => *v,
        _ => bail!("{}", rust_i18n::t!("error.missing_version")),
    };
    if version != 1 && version != SOURCES_VERSION {
        bail!(
            "{}",
            rust_i18n::t!("error.unsupported_version", version = version)
        );
    }

    let Some(toml::Value::Array(sources)) = root.get("source") else {
        bail!("{}", rust_i18n::t!("error.no_sources"));
    };

    let mut customs = HashMap::new();
    for (index, item) in sources.iter().enumerate() {
        let toml::Value::Table(entry) = item else {
            bail!(
                "{}",
                rust_i18n::t!("error.source_entry_format", index = index + 1)
            );
        };

        // v1 用 name 字段作为 id；v2 用 id 字段，name 为展示名
        let (id, default_name) = if version == 1 {
            let name = entry
                .get("name")
                .and_then(toml::Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .with_context(|| {
                    rust_i18n::t!("error.source_missing_name", index = index + 1).to_string()
                })?;
            (name.to_string(), name.to_string())
        } else {
            let id = entry
                .get("id")
                .and_then(toml::Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .with_context(|| {
                    rust_i18n::t!("error.source_missing_id", index = index + 1).to_string()
                })?;
            (id.to_string(), id.to_string())
        };

        let url = entry
            .get("url")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .with_context(|| {
                rust_i18n::t!("error.source_missing_url", id = id.as_str()).to_string()
            })?;

        let name = entry
            .get("name")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&default_name);
        let description = entry
            .get("description")
            .and_then(toml::Value::as_str)
            .unwrap_or("");

        customs.insert(id, CustomSource::new(name, url, description));
    }
    if customs.is_empty() {
        bail!("{}", rust_i18n::t!("error.no_valid_sources"));
    }
    Ok(customs)
}

/// 输出带转义的 TOML 基本字符串。
fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// 写入全局配置，写入前保留 `.bak` 备份与配置快照。
pub fn write_config(content: &str, source: &str) -> Result<PathBuf> {
    let path = config_path()?;
    write_config_scoped(&path, content, SCOPE_GLOBAL, source)?;
    Ok(path)
}

/// 写入项目级配置 `<项目根>/.cargo/config.toml`，同样保留 `.bak` 备份与快照。
pub fn write_project_config(root: &Path, content: &str, source: &str) -> Result<PathBuf> {
    let path = project_config_path(root);
    write_config_scoped(&path, content, SCOPE_PROJECT, source)?;
    Ok(path)
}

/// 用调用方已读取的现有内容计算合并结果（不落盘），用于写入前预览。
pub fn preview_config_from(existing: &str, content: &str) -> Result<String> {
    merge_config(existing, content)
}

/// 写入配置文件：内容有变化时先保存配置快照，再保留 `.bak` 并落盘。
fn write_config_scoped(path: &Path, content: &str, scope: &str, source: &str) -> Result<()> {
    let existing = read_optional(path)?;
    let merged = merge_config(&existing, content)?;
    commit_merged(path, scope, source, &existing, &merged)
}

/// 落盘合并后的内容：内容确实变化且原文件非空时先留一份快照。
fn commit_merged(
    path: &Path,
    scope: &str,
    source: &str,
    existing: &str,
    merged: &str,
) -> Result<()> {
    if existing != merged && !existing.trim().is_empty() {
        let _ = save_snapshot(path, scope, source, existing);
    }
    write_merged(path, merged)
}

/// 写入配置文件：自动创建父目录，并在覆盖前备份为 `.bak`。
///
/// `content` 只包含 srmgui 负责的键（`[source.*]` 与 `[net]`），写入时与
/// 现有文件合并，保留其中其他配置和注释（如链接器、`[build]`、用户自定义源），
/// 避免整文件覆盖造成误删。
#[cfg(test)]
fn write_config_file(path: &Path, content: &str) -> Result<()> {
    let merged = merge_config(&read_optional(path)?, content)?;
    write_merged(path, &merged)
}

/// 把合并后的完整内容写入目标文件，覆盖前保留 `.bak`。
fn write_merged(path: &Path, merged: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            rust_i18n::t!("error.create_dir", path = parent.display().to_string()).to_string()
        })?;
    }

    if path.exists() {
        let mut backup = path.as_os_str().to_os_string();
        backup.push(".bak");
        let _ = std::fs::copy(path, PathBuf::from(backup));
    }
    std::fs::write(path, merged).with_context(|| {
        rust_i18n::t!("error.write_file", path = path.display().to_string()).to_string()
    })?;
    Ok(())
}

/// 读取配置文件内容，文件不存在时返回空字符串（供快照对比当前配置使用）。
pub fn read_config_content(path: &Path) -> Result<String> {
    read_optional(path)
}

/// 写入健康检查修复后的完整内容：先保留快照，再备份 `.bak` 后落盘。
pub(crate) fn write_repaired(path: &Path, scope: &str, content: &str) -> Result<()> {
    let existing = read_optional(path)?;
    commit_merged(path, scope, "", &existing, content)
}

/// 读取文件内容，文件不存在时视为空内容。
fn read_optional(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(err).with_context(|| {
            rust_i18n::t!("error.read_file", path = path.display().to_string()).to_string()
        }),
    }
}

/// 把 srmgui 管理的键合并进现有内容并返回完整结果（不落盘）。
fn merge_config(existing: &str, content: &str) -> Result<String> {
    let managed: DocumentMut = content
        .parse()
        .with_context(|| rust_i18n::t!("error.parse_cargo_config").to_string())?;
    let mut merged = if existing.trim().is_empty() {
        DocumentMut::new()
    } else {
        // 现有文件无法解析时直接报错，不做覆盖以免丢失用户配置。
        existing
            .parse::<DocumentMut>()
            .with_context(|| rust_i18n::t!("error.parse_cargo_config").to_string())?
    };
    merge_managed(&mut merged, &managed);
    Ok(merged.to_string())
}

/// 保存一次配置快照，并清理超出保留数量的旧快照。
pub fn save_snapshot(target: &Path, scope: &str, source: &str, content: &str) -> Result<()> {
    let dir = snapshot_dir()?;
    write_snapshot_file(&dir, target, scope, source, content)?;
    prune_snapshots(&dir);
    Ok(())
}

/// 读取全部配置快照，按时间从新到旧排序。
pub fn read_snapshots() -> Vec<Snapshot> {
    snapshot_dir()
        .map(|dir| read_snapshots_in(&dir))
        .unwrap_or_default()
}

/// 把快照内容写回原来的配置文件，覆盖前保留 `.bak`。
pub fn restore_snapshot(snapshot: &Snapshot) -> Result<()> {
    snapshot
        .content
        .parse::<toml::Table>()
        .with_context(|| rust_i18n::t!("error.parse_cargo_config").to_string())?;
    // 复用统一的落盘逻辑：自动建目录并在覆盖前保留 `.bak`。
    write_merged(&snapshot.target, &snapshot.content)
}

/// 在指定目录写出一份快照文件。
fn write_snapshot_file(
    dir: &Path,
    target: &Path,
    scope: &str,
    source: &str,
    content: &str,
) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| {
        rust_i18n::t!("error.create_dir", path = dir.display().to_string()).to_string()
    })?;

    // 文件名带时间戳保证从新到旧可按名称排序，同一毫秒内追加序号避免覆盖。
    let stamp = now_millis();
    let file = (0u32..)
        .map(|index| dir.join(format!("snapshot-{stamp:013}-{index:03}.toml")))
        .find(|path| !path.exists())
        .expect("unbounded snapshot file search");

    let mut out = String::from("# srmgui 配置快照（由 srmgui 管理，请勿手动编辑）\n");
    out.push_str("version = 1\n");
    out.push_str(&format!(
        "created_at = {}\n",
        quote(&crate::logger::format_timestamp())
    ));
    out.push_str(&format!(
        "target = {}\n",
        quote(&target.display().to_string())
    ));
    out.push_str(&format!("scope = {}\n", quote(scope)));
    // 快照内容是写入前的配置，源按内容推导，保证行内标签与内容一致。
    let snapshot_source = source_in_config(content).unwrap_or_else(|| source.to_string());
    out.push_str(&format!("source = {}\n", quote(&snapshot_source)));
    out.push_str(&format!("content = {}\n", quote(content)));

    std::fs::write(&file, out).with_context(|| {
        rust_i18n::t!("error.write_file", path = file.display().to_string()).to_string()
    })?;
    Ok(())
}

/// 读取指定目录下的快照，按时间从新到旧排序。
fn read_snapshots_in(dir: &Path) -> Vec<Snapshot> {
    let mut files = snapshot_files(dir);
    files.sort();
    files.reverse();
    files
        .into_iter()
        .filter_map(|file| {
            let text = std::fs::read_to_string(&file).ok()?;
            parse_snapshot(&text)
        })
        .collect()
}

/// 解析单个快照文件，格式不正确时返回 `None`。
fn parse_snapshot(text: &str) -> Option<Snapshot> {
    let root: toml::Table = text.parse().ok()?;
    let field = |key: &str| {
        root.get(key)
            .and_then(toml::Value::as_str)
            .map(str::to_string)
    };
    Some(Snapshot {
        created_at: field("created_at")?,
        target: PathBuf::from(field("target")?),
        scope: field("scope").unwrap_or_else(|| SCOPE_GLOBAL.to_string()),
        // 旧快照文件里记录的可能是写入后的源，读取时一律以内容为准。
        source: source_in_config(&field("content")?)
            .or_else(|| field("source"))
            .unwrap_or_default(),
        content: field("content")?,
    })
}

/// 只保留最近 [`AppConfig::snapshot_limit`] 份快照（值来自用户设置，回退默认 10）。
fn prune_snapshots(dir: &Path) {
    let limit = cached_config().snapshot_limit;
    let mut files = snapshot_files(dir);
    files.sort();
    let excess = files.len().saturating_sub(limit);
    for file in files.into_iter().take(excess) {
        let _ = std::fs::remove_file(file);
    }
}

/// 目录下的快照文件列表。
fn snapshot_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("snapshot-"))
                && path.extension().is_some_and(|ext| ext == "toml")
        })
        .collect()
}

/// 当前时间的毫秒时间戳，用于快照文件名排序。
fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

/// 把 srmgui 管理的配置合并进现有文档，其他内容保持不变。
fn merge_managed(config: &mut DocumentMut, managed: &DocumentMut) {
    merge_table(config.as_table_mut(), managed.as_table());

    // `replace-with` 是唯一按需删除的键：选择官方源时生成内容里不含它，
    // 这里同步移除，否则旧的镜像替换设置会继续生效。
    let keep_replace = managed
        .get("source")
        .and_then(Item::as_table_like)
        .and_then(|table| table.get("crates-io"))
        .and_then(Item::as_table_like)
        .is_some_and(|table| table.contains_key("replace-with"));
    if !keep_replace {
        if let Some(crates_io) = config
            .get_mut("source")
            .and_then(Item::as_table_like_mut)
            .and_then(|table| table.get_mut("crates-io"))
            .and_then(Item::as_table_like_mut)
        {
            crates_io.remove("replace-with");
        }
    }
}

/// 递归合并：同名表继续向下合并，其余键直接覆盖。
fn merge_table(target: &mut dyn TableLike, source: &dyn TableLike) {
    for (key, item) in source.iter() {
        if let Some(table) = item.as_table_like() {
            merge_table(ensure_table(target, key), table);
        } else {
            target.insert(key, item.clone());
        }
    }
}

/// 取得（必要时创建）`target` 下的同名子表。
fn ensure_table<'a>(target: &'a mut dyn TableLike, key: &str) -> &'a mut dyn TableLike {
    if !target.get(key).is_some_and(Item::is_table_like) {
        target.insert(key, Item::Table(Table::new()));
    }
    target
        .get_mut(key)
        .and_then(Item::as_table_like_mut)
        .expect("just inserted table")
}

/// 解析现有配置并返回可编辑文档；空内容视为空文档。
fn parse_document(existing: &str) -> Result<DocumentMut> {
    if existing.trim().is_empty() {
        return Ok(DocumentMut::new());
    }
    existing
        .parse::<DocumentMut>()
        .with_context(|| rust_i18n::t!("error.parse_cargo_config").to_string())
}

/// 在现有内容中写入 `[registries.<name>] index`，其余内容保持不变。
fn merge_registry_entry(existing: &str, name: &str, index: &str) -> Result<String> {
    let mut doc = parse_document(existing)?;
    let registries = ensure_table(doc.as_table_mut(), "registries");
    ensure_table(registries, name).insert("index", toml_edit::value(index));
    Ok(doc.to_string())
}

/// 从现有内容中删除 `[registries.<name>]`，其余内容保持不变。
fn remove_registry_entry(existing: &str, name: &str) -> Result<String> {
    if existing.trim().is_empty() {
        return Ok(String::new());
    }
    let mut doc = parse_document(existing)?;
    if let Some(registries) = doc.get_mut("registries").and_then(Item::as_table_like_mut) {
        registries.remove(name);
    }
    Ok(doc.to_string())
}

/// 在 credentials 内容中写入 `[registries.<name>] token`。
fn merge_token_entry(existing: &str, name: &str, token: &str) -> Result<String> {
    let mut doc = parse_document(existing)?;
    let registries = ensure_table(doc.as_table_mut(), "registries");
    ensure_table(registries, name).insert("token", toml_edit::value(token));
    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_cache_is_invalidated_after_write() {
        let dir =
            std::env::temp_dir().join(format!("srmgui-settings-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        set_test_app_dir(Some(dir.clone()));

        let mut settings = read_settings();
        settings.probe_interval_secs = Some(77);
        write_settings(&settings).unwrap();
        assert_eq!(cached_config().probe_interval_secs, 77);

        settings.probe_interval_secs = Some(99);
        write_settings(&settings).unwrap();
        assert_eq!(
            cached_config().probe_interval_secs,
            99,
            "写入设置后运行时配置缓存必须失效"
        );

        set_test_app_dir(None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_time_is_shown_in_local_time() {
        let expected = chrono::NaiveDateTime::parse_from_str(
            "2026-11-16 10:00:00.000",
            "%Y-%m-%d %H:%M:%S%.3f",
        )
        .unwrap()
        .and_utc()
        .with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
        assert_eq!(snapshot_local_time("2026-11-16 10:00:00.000"), expected);
        // 无法解析的历史内容按原样展示，不能吞掉信息。
        assert_eq!(snapshot_local_time("t"), "t");
    }

    fn sample() -> HashMap<String, CustomSource> {
        HashMap::from([
            (
                "foo".to_string(),
                CustomSource::new("Foo 源", "sparse+https://foo.example/index/", "foo 说明"),
            ),
            (
                "bar".to_string(),
                CustomSource::new("Bar 源", "sparse+https://bar.example/index/", ""),
            ),
        ])
    }

    #[test]
    fn export_import_roundtrip() {
        let text = export_customs(&sample());
        assert!(text.contains("format = \"srmgui.sources\""));
        assert!(text.contains("version = 2"));
        assert!(text.contains("id = \"foo\""));
        assert!(text.contains("name = \"Foo 源\""));
        assert!(text.contains("description = \"foo 说明\""));
        assert_eq!(import_customs(&text).unwrap(), sample());
    }

    #[test]
    fn export_sorts_by_id() {
        let text = export_customs(&sample());
        let bar = text.find("id = \"bar\"").unwrap();
        let foo = text.find("id = \"foo\"").unwrap();
        assert!(bar < foo);
    }

    #[test]
    fn import_v1_backward_compat() {
        let text = "format = \"srmgui.sources\"\nversion = 1\n\n[[source]]\nname = \"legacy\"\nurl = \"sparse+https://legacy.example/index/\"\n";
        let customs = import_customs(text).unwrap();
        let src = customs.get("legacy").unwrap();
        assert_eq!(src.name, "legacy");
        assert_eq!(src.url, "sparse+https://legacy.example/index/");
        assert_eq!(src.description, "");
    }

    #[test]
    fn quote_escapes_special_characters() {
        assert_eq!(quote("a\"b\\c\nd"), "\"a\\\"b\\\\c\\nd\"");
    }

    #[test]
    fn rejects_foreign_files() {
        assert!(import_customs("[source.foo]\nregistry = \"x\"\n").is_err());
        assert!(import_customs("format = \"other\"\nversion = 1\n").is_err());
    }

    #[test]
    fn rejects_unsupported_version() {
        let text = "format = \"srmgui.sources\"\nversion = 99\n";
        assert!(import_customs(text).is_err());
    }

    #[test]
    fn rejects_entries_without_url() {
        let text = "format = \"srmgui.sources\"\nversion = 2\n\n[[source]]\nid = \"foo\"\nname = \"Foo\"\n";
        assert!(import_customs(text).is_err());
    }

    #[test]
    fn rejects_entries_without_id() {
        let text = "format = \"srmgui.sources\"\nversion = 2\n\n[[source]]\nname = \"Foo\"\nurl = \"sparse+https://x/\"\n";
        assert!(import_customs(text).is_err());
    }

    #[test]
    fn v2_entry_defaults_name_to_id_when_missing() {
        let text = "format = \"srmgui.sources\"\nversion = 2\n\n[[source]]\nid = \"foo\"\nurl = \"sparse+https://x/\"\n";
        let customs = import_customs(text).unwrap();
        assert_eq!(customs.get("foo").unwrap().name, "foo");
        assert_eq!(customs.get("foo").unwrap().description, "");
    }

    #[test]
    fn customs_metadata_roundtrip() {
        let text = "\n[[custom]]\nid = \"foo\"\nname = \"Foo 源\"\nurl = \"sparse+https://foo.example/index/\"\ndescription = \"说明\"\n";
        let customs = parse_customs_metadata(text).unwrap();
        let src = customs.get("foo").unwrap();
        assert_eq!(src.name, "Foo 源");
        assert_eq!(src.url, "sparse+https://foo.example/index/");
        assert_eq!(src.description, "说明");
    }

    #[test]
    fn project_config_path_is_under_dot_cargo() {
        let path = project_config_path(Path::new("demo"));
        assert_eq!(path, Path::new("demo").join(".cargo").join("config.toml"));
    }

    #[test]
    fn detects_project_root_from_nested_dir() {
        let base = std::env::temp_dir().join(format!("srmgui-root-{}", std::process::id()));
        let nested = base.join("crates").join("app").join("src");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(base.join("Cargo.toml"), "[workspace]\n").unwrap();

        assert_eq!(detect_project_root(&nested), Some(base.clone()));

        let lonely = std::env::temp_dir().join(format!("srmgui-root-none-{}", std::process::id()));
        std::fs::create_dir_all(&lonely).unwrap();
        assert_eq!(detect_project_root(&lonely), None);

        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&lonely);
    }

    #[test]
    fn current_source_at_reads_any_config_file() {
        let dir = std::env::temp_dir().join(format!("srmgui-current-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");

        assert_eq!(current_source_at(&path).unwrap(), None);

        std::fs::write(&path, "[source.crates-io]\nreplace-with = \"corp\"\n").unwrap();
        assert_eq!(current_source_at(&path).unwrap(), Some("corp".to_string()));

        std::fs::write(&path, "[net]\ngit-fetch-with-cli = true\n").unwrap();
        assert_eq!(
            current_source_at(&path).unwrap(),
            Some("official".to_string())
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_file_prefers_legacy_config() {
        let dir = std::env::temp_dir().join(format!("srmgui-legacy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(config_file_in(&dir), dir.join("config.toml"));

        std::fs::write(dir.join("config"), "[net]\n").unwrap();
        assert_eq!(config_file_in(&dir), dir.join("config"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_config_merges_and_preserves_other_sections() {
        let dir = std::env::temp_dir().join(format!("srmgui-merge-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "# 用户注释\n[target.x86_64-pc-windows-msvc]\nlinker = \"rust-lld.exe\"\n\n\
             [source.crates-io]\nreplace-with = \"old\"\n\n\
             [source.old]\nregistry = \"sparse+https://old.example/index/\"\n",
        )
        .unwrap();

        let managed = "[source.crates-io]\nreplace-with = \"rsproxy\"\n\n\
                       [source.rsproxy]\nregistry = \"sparse+https://rsproxy.cn/index/\"\n\n\
                       [net]\ngit-fetch-with-cli = true\n";
        write_config_file(&path, managed).unwrap();

        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("# 用户注释"));
        assert!(out.contains("linker = \"rust-lld.exe\""));
        assert!(out.contains("[source.old]"));
        assert!(out.contains("registry = \"sparse+https://old.example/index/\""));
        assert!(out.contains("replace-with = \"rsproxy\""));
        assert!(out.contains("registry = \"sparse+https://rsproxy.cn/index/\""));
        assert!(out.contains("git-fetch-with-cli = true"));
        assert!(!out.contains("replace-with = \"old\""));
        assert!(out.parse::<toml::Table>().is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn merge_removes_replace_with_when_official_selected() {
        let dir = std::env::temp_dir().join(format!("srmgui-official-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "[source.crates-io]\nreplace-with = \"rsproxy\"\n\n\
             [source.rsproxy]\nregistry = \"sparse+https://rsproxy.cn/index/\"\n",
        )
        .unwrap();

        write_config_file(
            &path,
            "[source.crates-io]\n\n[net]\ngit-fetch-with-cli = true\n",
        )
        .unwrap();

        let out = std::fs::read_to_string(&path).unwrap();
        assert!(!out.contains("replace-with"));
        // 其他源的配置保留
        assert!(out.contains("registry = \"sparse+https://rsproxy.cn/index/\""));
        assert!(out.contains("git-fetch-with-cli = true"));
        assert!(out.parse::<toml::Table>().is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_config_refuses_to_overwrite_invalid_file() {
        let dir = std::env::temp_dir().join(format!("srmgui-invalid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "not = [valid\n").unwrap();

        let managed = "[source.crates-io]\nreplace-with = \"rsproxy\"\n";
        assert!(write_config_file(&path, managed).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not = [valid\n");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preview_config_merges_without_writing() {
        let dir = std::env::temp_dir().join(format!("srmgui-preview-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let original = "[source.crates-io]\nreplace-with = \"old\"\n";
        std::fs::write(&path, original).unwrap();

        let preview = preview_config_from(
            &read_config_content(&path).unwrap(),
            "[source.crates-io]\nreplace-with = \"new\"\n",
        )
        .unwrap();
        assert!(preview.contains("replace-with = \"new\""));
        assert!(!preview.contains("replace-with = \"old\""));
        // 预览不能改动磁盘上的文件
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_roundtrip_reads_newest_first_and_prunes() {
        let dir = std::env::temp_dir().join(format!("srmgui-snap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("config.toml");

        let limit = AppConfig::default().snapshot_limit;
        for index in 0..(limit + 3) {
            let content = format!(
                "[source.crates-io]\nreplace-with = \"rsproxy\"\n\n[net]\nvalue = {index}\n"
            );
            write_snapshot_file(&dir, &target, SCOPE_GLOBAL, "rsproxy", &content).unwrap();
        }
        prune_snapshots(&dir);

        let snapshots = read_snapshots_in(&dir);
        assert_eq!(snapshots.len(), limit);
        // 最新写入的快照排在最前
        assert!(
            snapshots[0]
                .content
                .contains(&format!("value = {}", limit + 2))
        );
        assert_eq!(snapshots[0].target, target);
        assert_eq!(snapshots[0].scope, SCOPE_GLOBAL);
        assert_eq!(snapshots[0].source, "rsproxy");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_source_follows_content() {
        // 旧快照文件里记录的是写入后的源，读取时应以内容中的实际源为准。
        let text = "version = 1\ncreated_at = \"2026-11-16 10:00:00.000\"\ntarget = \"config.toml\"\nscope = \"global\"\nsource = \"aliyun\"\ncontent = \"[source.crates-io]\\nreplace-with = \\\"rsproxy\\\"\\n\"\n";
        assert_eq!(parse_snapshot(text).unwrap().source, "rsproxy");

        // 内容里没有 replace-with 时表示官方源。
        let official = "version = 1\ncreated_at = \"t\"\ntarget = \"config.toml\"\nscope = \"global\"\nsource = \"aliyun\"\ncontent = \"[net]\\ngit-fetch-with-cli = true\\n\"\n";
        assert_eq!(parse_snapshot(official).unwrap().source, "official");
    }

    #[test]
    fn restore_snapshot_writes_content_back() {
        let dir = std::env::temp_dir().join(format!("srmgui-restore-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("config.toml");
        std::fs::write(&target, "[source.crates-io]\nreplace-with = \"broken\"\n").unwrap();

        let snapshot = Snapshot {
            created_at: "2026-11-15 22:00:00.000".to_string(),
            target: target.clone(),
            scope: SCOPE_GLOBAL.to_string(),
            source: "rsproxy".to_string(),
            content: "[source.crates-io]\nreplace-with = \"rsproxy\"\n".to_string(),
        };
        restore_snapshot(&snapshot).unwrap();

        assert_eq!(std::fs::read_to_string(&target).unwrap(), snapshot.content);
        // 覆盖前保留 .bak
        assert!(dir.join("config.toml.bak").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_snapshot_rejects_invalid_content() {
        let snapshot = Snapshot {
            created_at: "2026-11-15 22:00:00.000".to_string(),
            target: PathBuf::from("config.toml"),
            scope: SCOPE_GLOBAL.to_string(),
            source: "rsproxy".to_string(),
            content: "not = [valid\n".to_string(),
        };
        assert!(restore_snapshot(&snapshot).is_err());
    }

    #[test]
    fn registry_entry_merge_preserves_other_sections() {
        let existing = "[source.crates-io]\nreplace-with = \"rsproxy\"\n\n\
                        [registries.old]\nindex = \"sparse+https://old.example/index/\"\n";
        let merged =
            merge_registry_entry(existing, "corp", "sparse+https://corp.example/index/").unwrap();

        assert!(merged.contains("replace-with = \"rsproxy\""));
        assert!(merged.contains("[registries.old]"));
        assert!(merged.contains("[registries.corp]"));
        assert!(merged.contains("index = \"sparse+https://corp.example/index/\""));
        assert!(merged.parse::<toml::Table>().is_ok());
    }

    #[test]
    fn registry_entry_remove_keeps_other_entries() {
        let existing = "[registries.corp]\nindex = \"sparse+https://corp.example/index/\"\n\n\
                        [registries.other]\nindex = \"sparse+https://other.example/index/\"\n";
        let merged = remove_registry_entry(existing, "corp").unwrap();

        assert!(!merged.contains("[registries.corp]"));
        assert!(merged.contains("[registries.other]"));
        assert!(merged.parse::<toml::Table>().is_ok());
    }

    #[test]
    fn token_entry_merge_and_remove_roundtrip() {
        let existing = "[registry]\ntoken = \"crates-io-token\"\n";
        let merged = merge_token_entry(existing, "corp", "secret-token").unwrap();
        assert!(merged.contains("[registry]"));
        assert!(merged.contains("[registries.corp]"));
        assert!(merged.contains("token = \"secret-token\""));

        let removed = remove_registry_entry(&merged, "corp").unwrap();
        assert!(!removed.contains("[registries.corp]"));
        assert!(removed.contains("[registry]"));
        assert!(removed.parse::<toml::Table>().is_ok());
    }

    #[test]
    fn registry_remove_on_missing_file_is_noop() {
        let dir = std::env::temp_dir().join(format!("srmgui-registry-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");

        let merged = remove_registry_entry(&read_optional(&path).unwrap(), "corp").unwrap();
        assert!(merged.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_private_registries_from_config() {
        let dir = std::env::temp_dir().join(format!("srmgui-private-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "[registries.srmgui-test-zeta]\nindex = \"sparse+https://zeta.example/index/\"\n\n\
             [registries.srmgui-test-alpha]\nindex = \"sparse+https://alpha.example/index/\"\ntoken = \"secret\"\n",
        )
        .unwrap();

        let registries = read_private_registries(&path).unwrap();
        assert_eq!(registries.len(), 2);
        assert_eq!(registries[0].name, "srmgui-test-alpha");
        assert_eq!(registries[0].index, "sparse+https://alpha.example/index/");
        assert!(registries[0].has_token);
        assert_eq!(registries[1].name, "srmgui-test-zeta");
        assert!(!registries[1].has_token);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
