use std::path::{Path, PathBuf};

use crate::config_io;

/// 规范化 Registry URL：缺少协议时补全为 sparse+https 并追加结尾斜杠。
pub(crate) fn normalize_registry_url(url: &str) -> String {
    let url = url.trim();
    if url.starts_with("sparse+") || url.starts_with("http") {
        url.to_string()
    } else {
        format!("sparse+https://{}/", url.trim_end_matches('/'))
    }
}

/// 规范化私有 registry 的 index URL。
///
/// 私有镜像既可能是 sparse 索引（`sparse+https://...`），也可能是 git 索引
/// （`https://...` / `ssh://...` / `git@...`），后两者不能强行加 `sparse+`。
pub(crate) fn normalize_registry_index(input: &str) -> String {
    let url = input.trim();
    if url.starts_with("sparse+") {
        if url.ends_with('/') {
            url.to_string()
        } else {
            format!("{url}/")
        }
    } else if url.contains("://") || url.starts_with("git@") {
        url.to_string()
    } else {
        format!("sparse+https://{}/", url.trim_end_matches('/'))
    }
}

/// 私有 registry 名称是否合法：仅允许字母、数字、连字符与下划线。
pub(crate) fn valid_registry_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// 补全导出文件的 `.srmgui` 扩展名。
pub(crate) fn with_sources_extension(path: &Path) -> PathBuf {
    if path.extension().is_some() {
        path.to_path_buf()
    } else {
        path.with_extension(config_io::SOURCES_EXTENSION)
    }
}

/// 用系统文件管理器打开指定目录（跨平台）。
pub(crate) fn open_in_file_manager(path: &Path) {
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
