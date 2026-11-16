//! 诊断包导出。
//!
//! 将当前日志、Cargo 配置、应用设置与环境信息打包成一个 zip 文件，
//! 便于用户反馈问题时一次性提供完整上下文。
//! 出于安全考虑，认证凭据（`credentials.toml`）不会被包含。
//!
//! ZIP 采用 store（不压缩）方式自行写入，避免为此引入额外依赖；
//! 产物可直接被系统自带解压工具打开。

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};

use crate::config_io;
use crate::logger;

/// 待写入 ZIP 的一个文件。
struct Entry {
    name: String,
    data: Vec<u8>,
}

/// 导出诊断包到 `target`，返回打包的文件数量。
///
/// `context` 为调用方补充的运行期信息（作用域、当前源、地区等），
/// 会以 `key: value` 的形式写入 `environment.txt`。
pub(crate) fn export_bundle(
    target: &Path,
    project_root: Option<&Path>,
    context: &[(String, String)],
) -> Result<usize> {
    let mut entries = Vec::new();

    for (name, path) in config_files(project_root) {
        if let Ok(data) = fs::read(&path) {
            entries.push(Entry { name, data });
        }
    }

    entries.push(Entry {
        name: "environment.txt".to_string(),
        data: environment_text(project_root, context).into_bytes(),
    });

    entries.extend(
        log_files()
            .into_iter()
            .map(|(name, data)| Entry { name, data }),
    );

    let count = entries.len();
    let archive = build_zip(&entries)?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create directory {}", parent.display()))?;
    }
    fs::write(target, archive).with_context(|| format!("write {}", target.display()))?;
    Ok(count)
}

/// 需要打包的配置文件：全局 / 项目 Cargo 配置、应用设置与自定义源元数据。
fn config_files(project_root: Option<&Path>) -> Vec<(String, PathBuf)> {
    let mut files = Vec::new();
    if let Ok(path) = config_io::config_path() {
        files.push(("config/global-cargo-config.toml".to_string(), path));
    }
    if let Some(root) = project_root {
        files.push((
            "config/project-cargo-config.toml".to_string(),
            config_io::project_config_path(root),
        ));
    }
    if let Ok(path) = config_io::settings_path() {
        files.push(("config/srmgui-settings.toml".to_string(), path));
    }
    if let Ok(path) = config_io::customs_path() {
        files.push(("config/srmgui-customs.toml".to_string(), path));
    }
    files
}

/// 收集日志目录下的当前日志与滚动归档文件，按文件名排序。
fn log_files() -> Vec<(String, Vec<u8>)> {
    let dir = logger::log_dir();
    let Ok(read_dir) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<(String, PathBuf)> = read_dir
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?.to_string();
            name.starts_with("srmgui.log").then_some((name, path))
        })
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
        .into_iter()
        .filter_map(|(name, path)| {
            fs::read(&path)
                .ok()
                .map(|data| (format!("logs/{name}"), data))
        })
        .collect()
}

/// 生成 `environment.txt` 的内容。
fn environment_text(project_root: Option<&Path>, context: &[(String, String)]) -> String {
    let mut text = String::new();
    text.push_str("srmgui diagnostics\n");
    text.push_str(&format!("generated_at: {}\n", logger::format_timestamp()));
    text.push_str(&format!("version: {}\n", env!("CARGO_PKG_VERSION")));
    text.push_str(&format!("os: {}\n", std::env::consts::OS));
    text.push_str(&format!("arch: {}\n", std::env::consts::ARCH));

    text.push_str("\npaths:\n");
    text.push_str(&describe_path("app_dir", config_io::app_dir().ok()));
    text.push_str(&describe_path(
        "global_config",
        config_io::config_path().ok(),
    ));
    text.push_str(&describe_path(
        "project_config",
        project_root.map(config_io::project_config_path),
    ));
    text.push_str(&describe_path("settings", config_io::settings_path().ok()));
    text.push_str(&describe_path("log_dir", Some(logger::log_dir())));

    text.push_str("\ncontext:\n");
    for (key, value) in context {
        text.push_str(&format!("{key}: {value}\n"));
    }

    text.push_str("\nnote: credentials.toml is intentionally excluded (contains tokens)\n");
    text
}

/// 输出 `label: path (exists: yes/no)` 形式的一行。
fn describe_path(label: &str, path: Option<PathBuf>) -> String {
    match path {
        Some(path) => format!(
            "{label}: {} (exists: {})\n",
            path.display(),
            if path.exists() { "yes" } else { "no" }
        ),
        None => format!("{label}: <unavailable>\n"),
    }
}

/// 用 store 方式生成 ZIP 归档字节。
fn build_zip(entries: &[Entry]) -> Result<Vec<u8>> {
    let (dos_time, dos_date) = dos_datetime();
    let mut archive: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();

    for entry in entries {
        let name = entry.name.replace('\\', "/");
        let name_bytes = name.as_bytes();
        let name_len =
            u16::try_from(name_bytes.len()).context("file name is too long for the zip archive")?;
        let size =
            u32::try_from(entry.data.len()).context("file is too large for the zip archive")?;
        let offset = u32::try_from(archive.len()).context("zip archive is too large")?;
        let crc = crc32(&entry.data);

        // 本地文件头
        archive.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        archive.extend_from_slice(&20u16.to_le_bytes());
        archive.extend_from_slice(&0x0800u16.to_le_bytes()); // 文件名为 UTF-8
        archive.extend_from_slice(&0u16.to_le_bytes()); // store
        archive.extend_from_slice(&dos_time.to_le_bytes());
        archive.extend_from_slice(&dos_date.to_le_bytes());
        archive.extend_from_slice(&crc.to_le_bytes());
        archive.extend_from_slice(&size.to_le_bytes());
        archive.extend_from_slice(&size.to_le_bytes());
        archive.extend_from_slice(&name_len.to_le_bytes());
        archive.extend_from_slice(&0u16.to_le_bytes());
        archive.extend_from_slice(name_bytes);
        archive.extend_from_slice(&entry.data);

        // 中央目录项
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&0x0800u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&dos_time.to_le_bytes());
        central.extend_from_slice(&dos_date.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&name_len.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes()); // extra
        central.extend_from_slice(&0u16.to_le_bytes()); // comment
        central.extend_from_slice(&0u16.to_le_bytes()); // disk
        central.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        central.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name_bytes);
    }

    let central_offset = u32::try_from(archive.len()).context("zip archive is too large")?;
    let central_size = u32::try_from(central.len()).context("zip archive is too large")?;
    let count = u16::try_from(entries.len()).context("too many files for the zip archive")?;
    archive.extend_from_slice(&central);

    // 中央目录结束记录
    archive.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    archive.extend_from_slice(&0u16.to_le_bytes());
    archive.extend_from_slice(&0u16.to_le_bytes());
    archive.extend_from_slice(&count.to_le_bytes());
    archive.extend_from_slice(&count.to_le_bytes());
    archive.extend_from_slice(&central_size.to_le_bytes());
    archive.extend_from_slice(&central_offset.to_le_bytes());
    archive.extend_from_slice(&0u16.to_le_bytes());

    if archive.len() > u32::MAX as usize {
        bail!("zip archive exceeds 4 GiB limit");
    }
    Ok(archive)
}

/// 由当前时间生成 ZIP 使用的 DOS 日期 / 时间。
fn dos_datetime() -> (u16, u16) {
    let stamp = logger::format_timestamp();
    let number = |range: std::ops::Range<usize>| -> u32 {
        stamp.get(range).and_then(|s| s.parse().ok()).unwrap_or(0)
    };
    let year = number(0..4).max(1980);
    let month = number(5..7).clamp(1, 12);
    let day = number(8..10).clamp(1, 31);
    let hour = number(11..13).min(23);
    let min = number(14..16).min(59);
    let sec = number(17..19).min(59);

    let date = (((year - 1980) as u16) << 9) | ((month as u16) << 5) | day as u16;
    let time = ((hour as u16) << 11) | ((min as u16) << 5) | (sec as u16 / 2);
    (time, date)
}

/// 标准 CRC-32（IEEE 802.3）校验值，ZIP 每个条目都需携带。
fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (index, slot) in table.iter_mut().enumerate() {
        let mut value = index as u32;
        for _ in 0..8 {
            value = if value & 1 != 0 {
                0xEDB8_8320 ^ (value >> 1)
            } else {
                value >> 1
            };
        }
        *slot = value;
    }

    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc = table[((crc ^ byte as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_standard_vector() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn zip_archive_has_expected_structure() {
        let archive = build_zip(&[Entry {
            name: "logs/srmgui.log".to_string(),
            data: b"hello".to_vec(),
        }])
        .expect("zip should be built");

        assert_eq!(&archive[0..4], b"PK\x03\x04");
        assert!(
            archive
                .windows(15)
                .any(|window| window == b"logs/srmgui.log")
        );
        let eocd = archive.len() - 22;
        assert_eq!(&archive[eocd..eocd + 4], b"PK\x05\x06");
        let count = u16::from_le_bytes([archive[eocd + 10], archive[eocd + 11]]);
        assert_eq!(count, 1);
    }

    #[test]
    fn environment_text_includes_context_and_version() {
        let text = environment_text(None, &[("scope".to_string(), "global".to_string())]);
        assert!(text.contains(&format!("version: {}", env!("CARGO_PKG_VERSION"))));
        assert!(text.contains("scope: global"));
        assert!(text.contains("credentials.toml"));
    }
}
