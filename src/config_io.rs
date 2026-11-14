use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;

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
