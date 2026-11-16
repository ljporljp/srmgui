//! 应用日志模块。
//!
//! - 日志格式遵循业内通用约定：`[YYYY-MM-DD HH:MM:SS.mmm] [LEVEL] [module] message`
//! - 日志文件归档到用户主目录下以应用名命名的目录：`<HOME>/srmgui/logs/srmgui.log`
//! - 归档策略采用业内标准的按大小滚动（size-based rotation）：
//!   * 单个日志文件最大 10 MB（可通过设置覆盖），超出后滚动；
//!   * 保留最近 5 个滚动文件（可通过设置覆盖），更旧的自动删除；
//!   * 启动时若现存日志已超限，先执行一次滚动。
//! - 内存中保留最近 1000 条日志（可通过设置覆盖），供 UI 面板展示。

use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config_io::{effective_config, read_settings};

/// 日志级别，从低到高排列。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    #[allow(dead_code)]
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
        }
    }
}

/// 一条日志记录。
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub timestamp: String,
    pub level: LogLevel,
    pub module: String,
    pub message: String,
}

impl LogEntry {
    /// 按业内标准格式输出单行日志。
    pub fn format(&self) -> String {
        format!(
            "[{}] [{}] [{}] {}",
            self.timestamp,
            self.level.as_str(),
            self.module,
            self.message
        )
    }
}

/// 从设置读取日志参数，回退默认值。
fn cfg() -> crate::config_io::AppConfig {
    effective_config(&read_settings())
}

/// 应用日志目录：`<HOME>/srmgui/logs`。
pub fn log_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join("srmgui").join("logs")
}

/// 当前写入的日志文件路径。
fn log_file_path() -> PathBuf {
    log_dir().join("srmgui.log")
}

/// 日志写入器内部状态。
struct LoggerInner {
    writer: Option<BufWriter<fs::File>>,
    path: PathBuf,
    bytes_written: u64,
    entries: Vec<LogEntry>,
}

impl LoggerInner {
    fn open(path: PathBuf) -> Self {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        // 启动时若现存日志已超限，先滚动一次。
        rotate_files(&path);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .expect("failed to open log file");
        let bytes_written = file.metadata().map(|m| m.len()).unwrap_or(0);
        Self {
            writer: Some(BufWriter::new(file)),
            path,
            bytes_written,
            entries: Vec::new(),
        }
    }

    fn write_entry(&mut self, level: LogLevel, module: &str, message: &str) {
        let entry = LogEntry {
            timestamp: format_timestamp(),
            level,
            module: module.to_string(),
            message: message.to_string(),
        };
        let line = entry.format();
        let bytes = line.len() as u64 + 1; // +1 换行符

        let log_size_limit = cfg().max_log_mb * 1024 * 1024;
        let max_entries = cfg().max_log_entries;

        // 若写入后会超限，先滚动再写。
        if self.bytes_written + bytes > log_size_limit {
            self.rotate();
        }

        if let Some(writer) = self.writer.as_mut() {
            let _ = writeln!(writer, "{line}");
            let _ = writer.flush();
        }
        self.bytes_written += bytes;

        // 内存缓冲：保留最近 N 条。
        self.entries.push(entry);
        if self.entries.len() > max_entries {
            let overflow = self.entries.len() - max_entries;
            self.entries.drain(0..overflow);
        }
    }

    /// 执行滚动：关闭当前文件、轮转归档文件、重新打开。
    fn rotate(&mut self) {
        // 先 flush 并关闭当前文件，确保 Windows 下可重命名。
        if let Some(mut writer) = self.writer.take() {
            let _ = writer.flush();
            drop(writer);
        }
        rotate_files(&self.path);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .expect("failed to reopen log file after rotation");
        self.writer = Some(BufWriter::new(file));
        self.bytes_written = 0;
    }
}

/// 线程安全的全局日志器。
pub struct Logger {
    inner: Mutex<LoggerInner>,
}

impl Logger {
    fn new() -> Self {
        Self {
            inner: Mutex::new(LoggerInner::open(log_file_path())),
        }
    }

    /// 写入一条日志。
    pub fn log(&self, level: LogLevel, module: &str, message: &str) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.write_entry(level, module, message);
        }
    }

    /// 获取内存中保留的日志（按时间正序）。
    pub fn entries(&self) -> Vec<LogEntry> {
        self.inner
            .lock()
            .map(|inner| inner.entries.clone())
            .unwrap_or_default()
    }

    /// 日志文件所在目录，供 UI 提示或打开目录使用。
    pub fn dir(&self) -> PathBuf {
        log_dir()
    }
}

use std::sync::OnceLock;

static LOGGER: OnceLock<Logger> = OnceLock::new();

/// 获取全局日志器单例。
pub fn logger() -> &'static Logger {
    LOGGER.get_or_init(Logger::new)
}

/// 便捷函数：DEBUG 级别。
#[allow(dead_code)]
pub fn debug(module: &str, message: impl AsRef<str>) {
    logger().log(LogLevel::Debug, module, message.as_ref());
}

/// 便捷函数：INFO 级别。
pub fn info(module: &str, message: impl AsRef<str>) {
    logger().log(LogLevel::Info, module, message.as_ref());
}

/// 便捷函数：WARN 级别。
pub fn warn(module: &str, message: impl AsRef<str>) {
    logger().log(LogLevel::Warn, module, message.as_ref());
}

/// 便捷函数：ERROR 级别。
pub fn error(module: &str, message: impl AsRef<str>) {
    logger().log(LogLevel::Error, module, message.as_ref());
}

/// 按大小滚动日志文件。
///
/// 当 `path` 指向的当前日志文件大小超过配置的上限时执行滚动：
/// `srmgui.log` -> `srmgui.log.1`，旧的 `.1` -> `.2`，以此类推，
/// 超过配置保留数量的最旧文件被删除。
fn rotate_files(path: &Path) {
    let log_size_limit = cfg().max_log_mb * 1024 * 1024;
    let max_rotated = cfg().max_rotated_logs;
    let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if size < log_size_limit {
        return;
    }

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("srmgui.log");

    // 先删除最旧的归档文件，腾出编号空间。
    let oldest = parent.join(format!("{stem}.{max_rotated}"));
    let _ = fs::remove_file(&oldest);

    // 从高到低依次轮转，避免覆盖。
    for i in (1..max_rotated).rev() {
        let src = parent.join(format!("{stem}.{i}"));
        let dst = parent.join(format!("{stem}.{}", i + 1));
        if src.exists() {
            let _ = fs::rename(&src, &dst);
        }
    }

    // 当前文件归档为 .1。
    let archived = parent.join(format!("{stem}.1"));
    let _ = fs::rename(path, &archived);
}

/// 生成 `YYYY-MM-DD HH:MM:SS.mmm` 格式的 UTC 时间戳。
pub(crate) fn format_timestamp() -> String {
    let now = SystemTime::now();
    let dur = now.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = dur.as_secs() as i64;
    let millis = dur.subsec_millis();

    let (year, month, day, hour, min, sec) = unix_to_ymdhms(secs);
    format!(
        "{year:04}-{month:02}-{day:02} {hour:02}:{min:02}:{sec:02}.{millis:03}"
    )
}

/// 将 UNIX 时间戳（秒）转换为 UTC 年月日时分秒。
fn unix_to_ymdhms(timestamp: i64) -> (i32, u32, u32, u32, u32, u32) {
    let days = timestamp.div_euclid(86400);
    let time = timestamp.rem_euclid(86400);
    let hour = (time / 3600) as u32;
    let min = ((time % 3600) / 60) as u32;
    let sec = (time % 60) as u32;
    let (year, month, day) = days_to_ymd(days);
    (year, month, day, hour, min, sec)
}

/// 将自 1970-01-01 起的天数转换为公历（年, 月, 日）。
///
/// 采用 Howard Hinnant 的公历转换算法，不依赖 chrono。
fn days_to_ymd(days_since_epoch: i64) -> (i32, u32, u32) {
    let z = days_since_epoch + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = (if month <= 2 { y + 1 } else { y }) as i32;
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_level_ordering() {
        assert!(LogLevel::Debug < LogLevel::Info);
        assert!(LogLevel::Info < LogLevel::Warn);
        assert!(LogLevel::Warn < LogLevel::Error);
    }

    #[test]
    fn entry_format_follows_convention() {
        let entry = LogEntry {
            timestamp: "2026-01-15 10:30:45.123".to_string(),
            level: LogLevel::Info,
            module: "app".to_string(),
            message: "started".to_string(),
        };
        assert_eq!(
            entry.format(),
            "[2026-01-15 10:30:45.123] [INFO] [app] started"
        );
    }

    #[test]
    fn timestamp_has_expected_shape() {
        let ts = format_timestamp();
        // YYYY-MM-DD HH:MM:SS.mmm 共 23 个字符
        assert_eq!(ts.len(), 23);
        assert!(ts.chars().nth(4) == Some('-'));
        assert!(ts.chars().nth(7) == Some('-'));
        assert!(ts.chars().nth(10) == Some(' '));
        assert!(ts.chars().nth(13) == Some(':'));
        assert!(ts.chars().nth(16) == Some(':'));
        assert!(ts.chars().nth(19) == Some('.'));
    }

    #[test]
    fn days_to_ymd_known_epoch() {
        // 1970-01-01
        assert_eq!(days_to_ymd(0), (1970, 1, 1));
        // 2000-01-01（距 epoch 10957 天）
        assert_eq!(days_to_ymd(10957), (2000, 1, 1));
        // 2026-11-15（距 epoch 20772 天）
        assert_eq!(days_to_ymd(20772), (2026, 11, 15));
    }

    #[test]
    fn log_dir_under_home() {
        let dir = log_dir();
        assert!(dir.ends_with("srmgui") || dir.ends_with("logs"));
        assert!(dir.to_string_lossy().contains("srmgui"));
    }
}
