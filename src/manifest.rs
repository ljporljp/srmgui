//! 内置源清单在线订阅与源分享链接。
//!
//! - 启动时（以及每个更新检查周期）从远端 JSON 拉取内置源清单，
//!   新增镜像源只需更新清单，无需发版；
//! - 拉取结果缓存到 `<HOME>/srmgui/srmgui-sources.json`，离线时继续可用；
//! - 提供 `srmgui://source?data=...` 分享链接，支持复制与导入源。

use std::collections::HashSet;
use std::path::PathBuf;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

use crate::config_io;
use crate::net;
use crate::registry;
use crate::util::normalize_registry_url;

/// 内置源清单的远端地址（仓库根目录的 `sources.json`）。
pub(crate) const MANIFEST_URL: &str =
    "https://raw.githubusercontent.com/ljporljp/srmgui/main/sources.json";

/// 清单响应允许的最大字节数。
const MAX_MANIFEST_BYTES: usize = 256 * 1024;

/// 分享链接的固定前缀。
const SHARE_PREFIX: &str = "srmgui://source?data=";

/// 清单 / 分享链接中的单个源。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ManifestSource {
    #[serde(default)]
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) description: String,
    #[serde(default)]
    pub(crate) url: String,
    /// 所属地区代码（`CN` / `GLOBAL`），用于按 IP 地区匹配推荐源。
    #[serde(default)]
    pub(crate) region: String,
    /// 所属省份代码（ISO 3166-2:CN，如 `ZJ`），用于同省优先推荐。
    #[serde(default)]
    pub(crate) province: String,
}

impl ManifestSource {
    pub(crate) fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        description: impl Into<String>,
        url: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            url: url.into(),
            region: String::new(),
            province: String::new(),
        }
    }
}

/// 远端清单文档：`{ "sources": [...] }`。
#[derive(Debug, Deserialize)]
struct ManifestDocument {
    #[serde(default)]
    sources: Vec<ManifestSource>,
}

/// 分享链接载荷文档。
#[derive(Debug, Serialize, Deserialize)]
struct ShareDocument {
    format: String,
    version: u32,
    sources: Vec<ManifestSource>,
}

/// 编译期内置的兜底清单：远端与缓存都不可用时使用。
pub(crate) fn fallback_sources() -> Vec<ManifestSource> {
    registry::builtins()
        .into_iter()
        .map(|r| ManifestSource {
            id: r.id.to_string(),
            name: String::new(),
            description: String::new(),
            url: r.url.to_string(),
            region: r.region.to_string(),
            province: r.province.to_string(),
        })
        .collect()
}

/// 当前可用的内置源清单：优先缓存，其次兜底。
pub(crate) fn current_sources() -> Vec<ManifestSource> {
    load_cached().unwrap_or_else(fallback_sources)
}

/// 清单缓存文件路径。
fn cache_path() -> Option<PathBuf> {
    config_io::app_dir()
        .ok()
        .map(|dir| dir.join("srmgui-sources.json"))
}

/// 读取本地缓存的清单，文件缺失或解析失败时返回 `None`。
pub(crate) fn load_cached() -> Option<Vec<ManifestSource>> {
    let text = std::fs::read_to_string(cache_path()?).ok()?;
    parse_manifest(&text).ok()
}

/// 从远端拉取清单并写入缓存，成功时返回清单内容。
pub(crate) fn fetch_remote() -> Result<Vec<ManifestSource>, String> {
    let (status, body) = net::fetch(MANIFEST_URL, "application/json", MAX_MANIFEST_BYTES)
        .map_err(|error| error.to_string())?;
    if !status.starts_with('2') {
        return Err(format!("HTTP {status}"));
    }
    let text = String::from_utf8_lossy(&body).to_string();
    let sources = parse_manifest(&text)?;

    if let Some(path) = cache_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // 缓存写入失败不影响本次清单生效，仅下次启动回退到兜底清单。
        let _ = std::fs::write(path, &text);
    }
    Ok(sources)
}

/// 解析清单 JSON：支持 `{ "sources": [...] }` 与裸数组两种形式。
pub(crate) fn parse_manifest(text: &str) -> Result<Vec<ManifestSource>, String> {
    let items = match serde_json::from_str::<Vec<ManifestSource>>(text) {
        Ok(items) => items,
        Err(_) => serde_json::from_str::<ManifestDocument>(text)
            .map(|document| document.sources)
            .map_err(|error| rust_i18n::t!("manifest.invalid", error = error).to_string())?,
    };

    let mut sources = Vec::new();
    let mut seen = HashSet::new();
    for item in items {
        let id = item.id.trim().to_string();
        let url = item.url.trim().to_string();
        if id.is_empty() || url.is_empty() || !seen.insert(id.clone()) {
            continue;
        }
        // 旧版清单没有地区字段：按编译期内置源补齐，保证推荐逻辑可用。
        let builtin = registry::builtins()
            .into_iter()
            .find(|builtin| builtin.id == id);
        let region = match item.region.trim().to_ascii_uppercase() {
            region if !region.is_empty() => region,
            _ => builtin
                .as_ref()
                .map(|builtin| builtin.region.to_string())
                .unwrap_or_default(),
        };
        let province = match item.province.trim().to_ascii_uppercase() {
            province if !province.is_empty() => province,
            _ => builtin
                .as_ref()
                .map(|builtin| builtin.province.to_string())
                .unwrap_or_default(),
        };
        sources.push(ManifestSource {
            id,
            name: item.name,
            description: item.description,
            url: normalize_registry_url(&url),
            region,
            province,
        });
    }

    if sources.is_empty() {
        return Err(rust_i18n::t!("manifest.empty").to_string());
    }
    Ok(sources)
}

/// 内置源的展示名称：优先本地化文案，其次清单提供的名称，最后回退标识名。
pub(crate) fn display_name(source: &ManifestSource) -> String {
    localized(format!("registry.{}.name", source.id))
        .or_else(|| (!source.name.is_empty()).then(|| source.name.clone()))
        .unwrap_or_else(|| source.id.clone())
}

/// 内置源的展示说明，回退顺序同 [`display_name`]。
pub(crate) fn display_description(source: &ManifestSource) -> String {
    localized(format!("registry.{}.description", source.id))
        .or_else(|| (!source.description.is_empty()).then(|| source.description.clone()))
        .unwrap_or_default()
}

/// 查询本地化文案，缺失（未配置该 key）时返回 `None`。
fn localized(key: String) -> Option<String> {
    let text = rust_i18n::t!(key).to_string();
    (text != key).then_some(text)
}

/// 为单个源生成分享链接（内容为 base64url 编码的 JSON）。
pub(crate) fn share_link(source: &ManifestSource) -> String {
    let document = ShareDocument {
        format: "srmgui.share".to_string(),
        version: 1,
        sources: vec![source.clone()],
    };
    let payload = serde_json::to_vec(&document).expect("share payload serializes");
    format!("{SHARE_PREFIX}{}", URL_SAFE_NO_PAD.encode(payload))
}

/// 解析分享链接，返回其中的源列表。
pub(crate) fn parse_share_link(link: &str) -> Result<Vec<ManifestSource>, String> {
    let link = link.trim();
    if !link.starts_with("srmgui://") {
        return Err(rust_i18n::t!("manifest.link_invalid").to_string());
    }
    let (_, data) = link
        .split_once("?data=")
        .ok_or_else(|| rust_i18n::t!("manifest.link_invalid").to_string())?;
    let bytes = URL_SAFE_NO_PAD
        .decode(data.trim().trim_end_matches('='))
        .map_err(|error| rust_i18n::t!("manifest.decode_failed", error = error).to_string())?;
    let text = String::from_utf8(bytes)
        .map_err(|error| rust_i18n::t!("manifest.decode_failed", error = error).to_string())?;
    parse_manifest(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_manifest_object() {
        let json = r#"{
            "format": "srmgui.manifest",
            "version": 1,
            "sources": [
                {"id": "rsproxy", "name": "RsProxy", "description": "fast", "url": "https://rsproxy.cn/index/"},
                {"id": "sample", "url": "sparse+https://mirror.example.com/index/"}
            ]
        }"#;
        let sources = parse_manifest(json).unwrap();
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].id, "rsproxy");
        assert_eq!(sources[0].name, "RsProxy");
        assert_eq!(sources[0].url, "https://rsproxy.cn/index/");
        // 缺少协议时补全 sparse+https。
        assert_eq!(sources[1].url, "sparse+https://mirror.example.com/index/");
        assert_eq!(sources[1].name, "");
        // 旧清单没有地区字段时按内置源补齐。
        assert_eq!(sources[0].region, "CN");
        assert_eq!(sources[0].province, "BJ");
        assert_eq!(sources[1].region, "");
        assert_eq!(sources[1].province, "");
    }

    #[test]
    fn keeps_explicit_region() {
        let json = r#"{"sources":[{"id":"mirror","url":"https://m.example.com/","region":"jp","province":"13"}]}"#;
        let sources = parse_manifest(json).unwrap();
        assert_eq!(sources[0].region, "JP");
        assert_eq!(sources[0].province, "13");
    }

    #[test]
    fn parses_bare_array_and_skips_invalid_entries() {
        let json = r#"[
            {"id": "a", "url": "sparse+https://a.example.com/"},
            {"id": "a", "url": "sparse+https://dup.example.com/"},
            {"id": "", "url": "sparse+https://empty.example.com/"},
            {"id": "b"}
        ]"#;
        let sources = parse_manifest(json).unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].url, "sparse+https://a.example.com/");
    }

    #[test]
    fn rejects_empty_manifest() {
        assert!(parse_manifest("{\"sources\":[]}").is_err());
        assert!(parse_manifest("not json").is_err());
        assert!(parse_manifest("[1,2]").is_err());
    }

    #[test]
    fn decodes_json_escapes() {
        let json = r#"[{"id": "a", "name": "quote \" slash \\ tab \t unicode \u4e2d\u6587", "url": "https://a.example.com/"}]"#;
        let sources = parse_manifest(json).unwrap();
        assert_eq!(sources[0].name, "quote \" slash \\ tab \t unicode 中文");
    }

    #[test]
    fn share_link_roundtrip() {
        let source = ManifestSource::new(
            "my-mirror",
            "我的镜像",
            "含特殊字符：\"引号\" 与 \\ 反斜杠",
            "sparse+https://mirror.example.com/index/",
        );
        let link = share_link(&source);
        assert!(link.starts_with(SHARE_PREFIX));
        let parsed = parse_share_link(&link).unwrap();
        assert_eq!(parsed, vec![source]);
    }

    #[test]
    fn rejects_bad_share_links() {
        assert!(parse_share_link("https://example.com").is_err());
        assert!(parse_share_link("srmgui://source?data=###").is_err());
    }

    #[test]
    fn fallback_manifest_contains_official() {
        let sources = fallback_sources();
        assert!(sources.iter().any(|source| source.id == "official"));
        assert!(sources.iter().all(|source| !source.url.is_empty()));
    }
}
