use std::path::{Path, PathBuf};

use crate::config_io;

/// 规范化 Registry URL：缺少协议时补全为 sparse+https 并追加结尾斜杠。
pub(crate) fn normalize_registry_url(url: &str) -> String {
    let url = url.trim();
    if url.starts_with("sparse+") || url.starts_with("http") {
        url.to_string()
    } else {
        format!("sparse+https://{url}/")
    }
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
