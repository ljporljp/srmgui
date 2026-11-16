//! 后台自动检查更新与安装包下载。
//!
//! - 通过 GitHub Releases API 获取最新版本号，与编译期版本比较；
//! - 仅当存在适配当前平台的安装包时才提示更新；
//! - 下载安装包到 `<HOME>/srmgui/updates`，随后交给系统执行安装。

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config_io;
use crate::logger;
use crate::net;

/// 最新版本查询接口。
const RELEASE_API: &str = "https://api.github.com/repos/ljporljp/srmgui/releases/latest";

/// 查询接口响应允许的最大字节数。
const MAX_RELEASE_JSON: usize = 512 * 1024;

/// 发现的可更新版本及其对应安装包。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Available {
    /// 最新版本号（不含前导 `v`）。
    pub(crate) version: String,
    /// 安装包文件名。
    pub(crate) asset_name: String,
    /// 安装包下载地址。
    pub(crate) asset_url: String,
}

/// 更新检查/下载在界面中的状态。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum UpdateState {
    /// 未发现更新（或尚未检查完成）。
    #[default]
    Idle,
    /// 正在检查。
    Checking,
    /// 发现新版本，等待用户点击安装。
    Available(Available),
    /// 正在下载安装包。
    Downloading(Available),
}

impl UpdateState {
    /// 当前待更新的版本信息。
    pub(crate) fn available(&self) -> Option<&Available> {
        match self {
            Self::Available(update) | Self::Downloading(update) => Some(update),
            _ => None,
        }
    }
}

/// 后台检查是否有新版本：有新版本且存在当前平台的安装包时返回其信息。
pub(crate) fn check() -> Result<Option<Available>, String> {
    let (status, body) = net::fetch(RELEASE_API, "application/vnd.github+json", MAX_RELEASE_JSON)
        .map_err(|error| error.to_string())?;
    if !status.starts_with('2') {
        return Err(format!("HTTP {status}"));
    }

    let json = String::from_utf8_lossy(&body);
    let tag = net::json_string_field(&json, "tag_name")
        .ok_or_else(|| rust_i18n::t!("update.bad_response").to_string())?;
    let version = tag.trim_start_matches(['v', 'V']).to_string();
    if !is_newer(env!("CARGO_PKG_VERSION"), &version) {
        return Ok(None);
    }

    let assets = parse_assets(&json);
    let (asset_name, asset_url) = pick_asset(&assets, platform_suffixes()).ok_or_else(|| {
        rust_i18n::t!("update.no_asset", version = version.as_str()).to_string()
    })?;

    Ok(Some(Available {
        version,
        asset_name: asset_name.clone(),
        asset_url: asset_url.clone(),
    }))
}

/// 下载安装包到应用目录下的 `updates` 子目录，返回本地路径。
pub(crate) fn download(update: &Available) -> Result<PathBuf, String> {
    let dir = config_io::app_dir()
        .map_err(|error| format!("{error:#}"))?
        .join("updates");
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let path = dir.join(&update.asset_name);
    let bytes = net::download(&update.asset_url, &path).map_err(|error| error.to_string())?;
    logger::info(
        "update",
        rust_i18n::t!(
            "update.log_downloaded",
            path = path.display().to_string()
        )
        .to_string(),
    );
    logger::debug("update", format!("installer size: {bytes} bytes"));
    Ok(path)
}

/// 启动下载好的安装程序。
pub(crate) fn launch(path: &Path) -> Result<(), String> {
    let error_message = |error: std::io::Error| {
        rust_i18n::t!("update.install_failed", error = error.to_string()).to_string()
    };

    #[cfg(target_os = "windows")]
    {
        Command::new(path).spawn().map_err(error_message)?;
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg(path).spawn().map_err(error_message)?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        use std::os::unix::fs::PermissionsExt;
        let is_appimage = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("AppImage"))
            .unwrap_or(false);
        if is_appimage {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
            Command::new(path).spawn().map_err(error_message)?;
        } else {
            Command::new("xdg-open").arg(path).spawn().map_err(error_message)?;
        }
    }

    Ok(())
}

/// 当前平台安装包的文件名后缀，按优先级排列。
fn platform_suffixes() -> &'static [&'static str] {
    if cfg!(target_os = "windows") {
        &["windows-x64-setup.exe"]
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            &["macos-arm64.dmg"]
        } else {
            &["macos-x64.dmg"]
        }
    } else {
        &["linux-x64.AppImage", "linux-x64.deb"]
    }
}

/// 从资源列表中挑选当前平台安装包（文件名为下载地址的最后一段）。
fn pick_asset<'a>(
    assets: &'a [(String, String)],
    suffixes: &[&str],
) -> Option<&'a (String, String)> {
    suffixes
        .iter()
        .find_map(|suffix| assets.iter().find(|(name, _)| name.ends_with(suffix)))
}

/// 提取 `browser_download_url` 列表，文件名取自 URL 最后一段。
fn parse_assets(json: &str) -> Vec<(String, String)> {
    const KEY: &str = "\"browser_download_url\"";
    let mut assets = Vec::new();
    let mut search = json;
    while let Some(position) = search.find(KEY) {
        let rest = search[position + KEY.len()..].trim_start();
        let Some(rest) = rest.strip_prefix(':') else {
            break;
        };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('"') else {
            break;
        };
        let Some((url, consumed)) = net::scan_json_string(rest) else {
            break;
        };
        let name = url.rsplit('/').next().unwrap_or_default().to_string();
        if !name.is_empty() {
            assets.push((name, url));
        }
        search = &rest[consumed..];
    }
    assets
}

/// 解析 `1.2.3` / `v1.2.3` 形式的版本号。
fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let core = text.trim().trim_start_matches(['v', 'V']);
    let core = core.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

/// 判断 `latest` 是否比 `current` 更新。
pub(crate) fn is_newer(current: &str, latest: &str) -> bool {
    match (parse_version(current), parse_version(latest)) {
        (Some(current), Some(latest)) => latest > current,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("1.2.2", "1.2.3"));
        assert!(is_newer("1.2.2", "v1.3.0"));
        assert!(is_newer("1.2.2", "2.0.0"));
        assert!(!is_newer("1.2.2", "1.2.2"));
        assert!(!is_newer("1.2.2", "1.2.1"));
        assert!(!is_newer("1.2.2", "not-a-version"));
    }

    #[test]
    fn parses_and_picks_assets() {
        let json = r#"{"tag_name":"v1.3.0","assets":[
            {"name":"srmgui-1.3.0-windows-x64-setup.exe","browser_download_url":"https://example.com/srmgui-1.3.0-windows-x64-setup.exe"},
            {"name":"srmgui-1.3.0-linux-x64.AppImage","browser_download_url":"https://example.com/srmgui-1.3.0-linux-x64.AppImage"}
        ]}"#;
        let assets = parse_assets(json);
        assert_eq!(assets.len(), 2);
        assert_eq!(assets[0].0, "srmgui-1.3.0-windows-x64-setup.exe");

        let picked = pick_asset(&assets, &["windows-x64-setup.exe"]).unwrap();
        assert_eq!(
            picked.1,
            "https://example.com/srmgui-1.3.0-windows-x64-setup.exe"
        );
        assert!(pick_asset(&assets, &["macos-arm64.dmg"]).is_none());
    }

    /// 需要联网，默认忽略：`cargo test -- --ignored real_release_api`。
    #[test]
    #[ignore]
    fn real_release_api() {
        let result = check();
        println!("release check: {result:?}");
    }
}
