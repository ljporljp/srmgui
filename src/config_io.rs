use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::path::PathBuf;

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

/// Cargo home: `$CARGO_HOME` or `~/.cargo`.
pub fn cargo_home() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("CARGO_HOME") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .context("cannot find home directory")?;
    Ok(PathBuf::from(home).join(".cargo"))
}

/// Path to the active cargo config file (prefers `config.toml` over legacy `config`).
pub fn config_path() -> Result<PathBuf> {
    let home = cargo_home()?;
    let toml = home.join("config.toml");
    let legacy = home.join("config");
    Ok(if toml.exists() { toml } else { legacy })
}

/// 自定义源元数据文件路径（存放 name / description 等 cargo config 无法承载的信息）。
pub fn customs_path() -> Result<PathBuf> {
    Ok(cargo_home()?.join("srmgui-customs.toml"))
}

/// Read which source id is currently in effect.
///
/// `replace-with` 决定被替换成的源；没有该字段时 cargo 直接使用默认的
/// crates.io（即内置的 `official`），因此返回 `official` 而不是 `None`。
/// 只有配置文件不存在（从未写入过配置）时才返回 `None`。
pub fn current_source() -> Result<Option<String>> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let root: toml::Table = text.parse().context("parse cargo config")?;

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
    let root: toml::Table = text.parse().context("解析 srmgui 元数据文件失败")?;
    let Some(toml::Value::Array(customs)) = root.get("custom") else {
        return Ok(HashMap::new());
    };

    let mut out = HashMap::new();
    for (index, item) in customs.iter().enumerate() {
        let toml::Value::Table(entry) = item else {
            bail!("元数据第 {} 条目格式错误", index + 1);
        };
        let id = entry
            .get("id")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .with_context(|| format!("元数据第 {} 条缺少 id", index + 1))?;
        let url = entry
            .get("url")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .with_context(|| format!("元数据「{id}」缺少 url"))?;
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

/// 持久化自定义源元数据到 `~/.cargo/srmgui-customs.toml`。
pub fn write_customs(customs: &HashMap<String, CustomSource>) -> Result<()> {
    let path = customs_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| parent.display().to_string())?;
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
    std::fs::write(&path, out).with_context(|| path.display().to_string())?;
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
    let root: toml::Table = text.parse().context("解析 srmgui 文件失败")?;

    match root.get("format") {
        Some(toml::Value::String(format)) if format == SOURCES_FORMAT => {}
        _ => bail!("不是 srmgui 源文件（缺少 format = \"{SOURCES_FORMAT}\"）"),
    }
    let version = match root.get("version") {
        Some(toml::Value::Integer(v)) => *v,
        _ => bail!("srmgui 文件缺少 version 字段"),
    };
    if version != 1 && version != SOURCES_VERSION {
        bail!("不支持的 srmgui 文件版本：{version}");
    }

    let Some(toml::Value::Array(sources)) = root.get("source") else {
        bail!("srmgui 文件中没有源条目");
    };

    let mut customs = HashMap::new();
    for (index, item) in sources.iter().enumerate() {
        let toml::Value::Table(entry) = item else {
            bail!("第 {} 个源条目格式错误", index + 1);
        };

        // v1 用 name 字段作为 id；v2 用 id 字段，name 为展示名
        let (id, default_name) = if version == 1 {
            let name = entry
                .get("name")
                .and_then(toml::Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .with_context(|| format!("第 {} 个源缺少 name", index + 1))?;
            (name.to_string(), name.to_string())
        } else {
            let id = entry
                .get("id")
                .and_then(toml::Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .with_context(|| format!("第 {} 个源缺少 id", index + 1))?;
            (id.to_string(), id.to_string())
        };

        let url = entry
            .get("url")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .with_context(|| format!("源「{id}」缺少 url"))?;

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
        bail!("srmgui 文件中没有有效的源条目");
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

/// Write the new config, keeping a `.bak` of the previous file.
pub fn write_config(content: &str) -> Result<PathBuf> {
    let home = cargo_home()?;
    std::fs::create_dir_all(&home).with_context(|| home.display().to_string())?;
    let path = home.join("config.toml");
    if path.exists() {
        let _ = std::fs::copy(&path, home.join("config.toml.bak"));
    }
    std::fs::write(&path, content).with_context(|| path.display().to_string())?;
    Ok(path)
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
}
