use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::path::PathBuf;

/// 自定义源导出文件的扩展名。
pub const SOURCES_EXTENSION: &str = "srmgui";

/// 导出文件的格式标识。
const SOURCES_FORMAT: &str = "srmgui.sources";

/// 导出文件的格式版本。
const SOURCES_VERSION: i64 = 1;

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

/// Read which source id is referenced by `[source.crates-io] replace-with`.
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
    Ok(None)
}

/// Read user-defined registries (id -> registry url) from cargo config.
pub fn read_customs() -> Result<HashMap<String, String>> {
    let mut customs = HashMap::new();
    let path = config_path()?;
    if !path.exists() {
        return Ok(customs);
    }
    let text = std::fs::read_to_string(&path).context("read cargo config")?;
    let root: toml::Table = match text.parse() {
        Ok(t) => t,
        Err(_) => return Ok(customs),
    };
    if let Some(toml::Value::Table(src)) = root.get("source") {
        for (k, v) in src {
            if k == "crates-io" {
                continue;
            }
            if let toml::Value::Table(t) = v {
                if let Some(toml::Value::String(url)) = t.get("registry") {
                    customs.insert(k.clone(), url.clone());
                }
            }
        }
    }
    Ok(customs)
}

/// 把自定义源导出为 srmgui 定义的文本格式（TOML 子集，按标识名排序）。
pub fn export_customs(customs: &HashMap<String, String>) -> String {
    let mut names: Vec<&String> = customs.keys().collect();
    names.sort();

    let mut out = String::from("# srmgui 自定义源文件，可通过 srmgui 导入\n");
    out.push_str(&format!("format = {}\n", quote(SOURCES_FORMAT)));
    out.push_str(&format!("version = {}\n", SOURCES_VERSION));
    for name in names {
        out.push_str("\n[[source]]\n");
        out.push_str(&format!("name = {}\n", quote(name)));
        out.push_str(&format!("url = {}\n", quote(&customs[name])));
    }
    out
}

/// 解析 srmgui 自定义源文件。
pub fn import_customs(text: &str) -> Result<HashMap<String, String>> {
    let root: toml::Table = text.parse().context("解析 srmgui 文件失败")?;

    match root.get("format") {
        Some(toml::Value::String(format)) if format == SOURCES_FORMAT => {}
        _ => bail!("不是 srmgui 源文件（缺少 format = \"{SOURCES_FORMAT}\"）"),
    }
    match root.get("version") {
        Some(toml::Value::Integer(version)) if *version == SOURCES_VERSION => {}
        Some(toml::Value::Integer(version)) => bail!("不支持的 srmgui 文件版本：{version}"),
        _ => bail!("srmgui 文件缺少 version 字段"),
    }

    let Some(toml::Value::Array(sources)) = root.get("source") else {
        bail!("srmgui 文件中没有源条目");
    };

    let mut customs = HashMap::new();
    for (index, item) in sources.iter().enumerate() {
        let toml::Value::Table(entry) = item else {
            bail!("第 {} 个源条目格式错误", index + 1);
        };
        let name = entry
            .get("name")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .with_context(|| format!("第 {} 个源缺少 name", index + 1))?;
        let url = entry
            .get("url")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .with_context(|| format!("源「{name}」缺少 url"))?;
        customs.insert(name.to_string(), url.to_string());
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

    fn sample() -> HashMap<String, String> {
        HashMap::from([
            (
                "foo".to_string(),
                "sparse+https://foo.example/index/".to_string(),
            ),
            (
                "bar".to_string(),
                "sparse+https://bar.example/index/".to_string(),
            ),
        ])
    }

    #[test]
    fn export_import_roundtrip() {
        let text = export_customs(&sample());
        assert!(text.contains("format = \"srmgui.sources\""));
        assert!(text.contains("version = 1"));
        assert_eq!(import_customs(&text).unwrap(), sample());
    }

    #[test]
    fn export_sorts_by_name() {
        let text = export_customs(&sample());
        let bar = text.find("name = \"bar\"").unwrap();
        let foo = text.find("name = \"foo\"").unwrap();
        assert!(bar < foo);
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
        let text = "format = \"srmgui.sources\"\nversion = 1\n\n[[source]]\nname = \"foo\"\n";
        assert!(import_customs(text).is_err());
    }

    #[test]
    fn rejects_entries_without_name() {
        let text =
            "format = \"srmgui.sources\"\nversion = 1\n\n[[source]]\nurl = \"sparse+https://x/\"\n";
        assert!(import_customs(text).is_err());
    }
}
