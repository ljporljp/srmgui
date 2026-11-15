use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, Table, TableLike};

/// 自定义源导出文件的扩展名。
pub const SOURCES_EXTENSION: &str = "srmgui";

/// 导出文件的格式标识。
const SOURCES_FORMAT: &str = "srmgui.sources";

/// 导出文件的格式版本（v2 起支持 name / description 字段）。
const SOURCES_VERSION: i64 = 2;

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
    pub fn new(name: impl Into<String>, url: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            url: url.into(),
            description: description.into(),
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

/// 应用配置目录：`<HOME>/srmgui`，与日志目录 `<HOME>/srmgui/logs` 同级。
pub fn app_dir() -> Result<PathBuf> {
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

/// 自定义源元数据文件路径（存放 name / description 等 cargo config 无法承载的信息）。
pub fn customs_path() -> Result<PathBuf> {
    Ok(app_dir()?.join("srmgui-customs.toml"))
}

/// 应用设置文件路径。
pub fn settings_path() -> Result<PathBuf> {
    Ok(app_dir()?.join("srmgui-settings.toml"))
}

/// 应用设置：界面语言与配置写入作用域。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    /// 持久化的界面语言（如 `zh-CN` / `en-US`）。
    pub language: Option<String>,
    /// 上次选中的项目根目录（含 `Cargo.toml` 的目录）。
    pub project_root: Option<PathBuf>,
    /// 是否写入项目级配置（`<项目根>/.cargo/config.toml`）。
    pub project_scope: bool,
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
    Settings {
        language: root
            .get("language")
            .and_then(toml::Value::as_str)
            .map(str::to_string),
        project_root: root
            .get("project_root")
            .and_then(toml::Value::as_str)
            .map(PathBuf::from),
        project_scope: root.get("scope").and_then(toml::Value::as_str) == Some("project"),
    }
}

/// 持久化应用设置。
pub fn write_settings(settings: &Settings) -> Result<()> {
    let path = settings_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            rust_i18n::t!("error.create_dir", path = parent.display().to_string()).to_string()
        })?;
    }
    let mut content = String::from("# srmgui 设置（由 srmgui 管理，请勿手动编辑）\n");
    if let Some(code) = &settings.language {
        content.push_str(&format!("language = {}\n", quote(code)));
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
    std::fs::write(&path, content).with_context(|| {
        rust_i18n::t!("error.write_file", path = path.display().to_string()).to_string()
    })?;
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
    let root: toml::Table = text
        .parse()
        .with_context(|| rust_i18n::t!("error.parse_cargo_config").to_string())?;

    // `[source.crates-io] replace-with = "..."`
    if let Some(toml::Value::Table(src)) = root.get("source") {
        if let Some(toml::Value::Table(cio)) = src.get("crates-io") {
            if let Some(toml::Value::String(s)) = cio.get("replace-with") {
                return Ok(Some(s.clone()));
            }
        }
    }
    Ok(Some("official".to_string()))
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
    if let Ok(path) = config_path() {
        if path.exists() {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(root) = text.parse::<toml::Table>() {
                    if let Some(toml::Value::Table(src)) = root.get("source") {
                        for (k, v) in src {
                            if k == "crates-io" {
                                continue;
                            }
                            if let toml::Value::Table(t) = v {
                                if let Some(toml::Value::String(url)) = t.get("registry") {
                                    customs.insert(
                                        k.clone(),
                                        CustomSource::new(k.clone(), url.clone(), ""),
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // 2. 用 srmgui 元数据文件覆盖 name / description
    if let Ok(path) = customs_path() {
        if path.exists() {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(meta) = parse_customs_metadata(&text) {
                    for (id, src) in meta {
                        customs.insert(id, src);
                    }
                }
            }
        }
    }

    Ok(customs)
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
            .with_context(|| {
                rust_i18n::t!("error.metadata_missing_url", id = id).to_string()
            })?;
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

/// Write the new global config, keeping a `.bak` of the previous file.
pub fn write_config(content: &str) -> Result<PathBuf> {
    let path = config_path()?;
    write_config_file(&path, content)?;
    Ok(path)
}

/// 写入项目级配置 `<项目根>/.cargo/config.toml`，同样保留 `.bak` 备份。
pub fn write_project_config(root: &Path, content: &str) -> Result<PathBuf> {
    let path = project_config_path(root);
    write_config_file(&path, content)?;
    Ok(path)
}

/// 写入配置文件：自动创建父目录，并在覆盖前备份为 `.bak`。
///
/// `content` 只包含 srmgui 负责的键（`[source.*]` 与 `[net]`），写入时与
/// 现有文件合并，保留其中其他配置和注释（如链接器、`[build]`、用户自定义源），
/// 避免整文件覆盖造成误删。
fn write_config_file(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            rust_i18n::t!("error.create_dir", path = parent.display().to_string()).to_string()
        })?;
    }

    let existing = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => {
            return Err(err).with_context(|| {
                rust_i18n::t!("error.read_file", path = path.display().to_string()).to_string()
            });
        }
    };

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

    if path.exists() {
        let mut backup = path.as_os_str().to_os_string();
        backup.push(".bak");
        let _ = std::fs::copy(path, PathBuf::from(backup));
    }
    std::fs::write(path, merged.to_string()).with_context(|| {
        rust_i18n::t!("error.write_file", path = path.display().to_string()).to_string()
    })?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

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
        let text =
            "format = \"srmgui.sources\"\nversion = 2\n\n[[source]]\nname = \"Foo\"\nurl = \"sparse+https://x/\"\n";
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
        assert_eq!(current_source_at(&path).unwrap(), Some("official".to_string()));

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

        write_config_file(&path, "[source.crates-io]\n\n[net]\ngit-fetch-with-cli = true\n").unwrap();

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
}
