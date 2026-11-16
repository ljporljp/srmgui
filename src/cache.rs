//! Cargo 缓存管理：统计 `CARGO_HOME` 下 registry / git 缓存体积，并清理无效缓存。
//!
//! 无效缓存均来自 cargo 的目录约定，清理后 cargo 会按需重新下载：
//! - `registry/src/<registry>/<crate-version>` 缺少 `.cargo-ok`：解压被中断；
//! - `git/checkouts/<repo>/<rev>` 找不到对应的 `git/db/<repo>`：工作副本已失主；
//! - `registry/{cache,src}/<registry>` 找不到对应的 `registry/index/<registry>`：
//!   没有索引时这些缓存无法再被 cargo 使用。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config_io;

/// 参与统计的缓存类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Category {
    RegistryIndex,
    RegistryCache,
    RegistrySrc,
    GitDb,
    GitCheckouts,
}

/// 缓存来源类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum SourceKind {
    Registry,
    Git,
}

/// 来源内某一缓存类别的体积。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SourcePart {
    pub category: Category,
    pub bytes: u64,
}

/// 一个缓存来源：一个 registry 镜像站或一个 git 仓库。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Source {
    pub kind: SourceKind,
    pub name: String,
    /// 组成该来源的实际缓存目录（registry 可能对应多个哈希目录）。
    pub paths: Vec<PathBuf>,
    pub bytes: u64,
    pub parts: Vec<SourcePart>,
    /// registry 的完整索引 URL（可还原时）或 git 仓库的远端地址。
    pub url: Option<String>,
}

/// 无效缓存类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum InvalidKind {
    /// 解压未完成（缺少 `.cargo-ok`）的源码目录。
    IncompleteSrc,
    /// 找不到对应 `git/db` 仓库的 git 工作副本。
    OrphanCheckout,
    /// 找不到对应索引的 registry 缓存目录。
    OrphanRegistry,
}

/// 一条无效缓存。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct InvalidEntry {
    pub kind: InvalidKind,
    pub path: PathBuf,
    pub bytes: u64,
}

/// 一次缓存扫描的结果。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct CacheReport {
    pub home: PathBuf,
    /// 按来源（registry 索引 / git 仓库）统计的明细，按体积从大到小排列。
    pub sources: Vec<Source>,
    pub invalid: Vec<InvalidEntry>,
    pub registry_bytes: u64,
    pub git_bytes: u64,
    /// 统计时缓存目录的变更指纹，用于判断结果是否仍然有效。
    pub fingerprint: String,
}

impl CacheReport {
    pub(crate) fn total_bytes(&self) -> u64 {
        self.registry_bytes + self.git_bytes
    }

    pub(crate) fn invalid_bytes(&self) -> u64 {
        self.invalid.iter().map(|entry| entry.bytes).sum()
    }
}

/// 一次清理的结果。
#[derive(Debug, Clone, Default)]
pub(crate) struct CleanReport {
    /// 成功删除的条目数量。
    pub removed: usize,
    /// 删除失败的条目数量。
    pub failed: usize,
    /// 成功释放的字节数。
    pub bytes: u64,
}

/// 缓存管理对话框展示的状态。
#[derive(Debug, Clone)]
pub(crate) enum CacheState {
    Idle,
    Scanning,
    Cleaning,
    Ready(CacheReport),
    Failed(String),
}

/// 扫描真实的 `CARGO_HOME`。
pub(crate) fn scan() -> Result<CacheReport> {
    Ok(scan_at(&config_io::cargo_home()?))
}

/// 统计指定 cargo home 下的 registry / git 缓存体积与无效缓存。
pub(crate) fn scan_at(home: &Path) -> CacheReport {
    scan_at_with_urls(home, &configured_index_urls())
}

/// 统计指定 cargo home 下的缓存；`index_urls` 为已知的索引地址，用于还原目录对应的完整 URL。
pub(crate) fn scan_at_with_urls(home: &Path, index_urls: &[String]) -> CacheReport {
    let sources = collect_sources(home, index_urls);
    let registry_bytes = sources
        .iter()
        .filter(|source| source.kind == SourceKind::Registry)
        .map(|source| source.bytes)
        .sum();
    let git_bytes = sources
        .iter()
        .filter(|source| source.kind == SourceKind::Git)
        .map(|source| source.bytes)
        .sum();

    CacheReport {
        home: home.to_path_buf(),
        sources,
        invalid: find_invalid(home),
        registry_bytes,
        git_bytes,
        fingerprint: fingerprint(home),
    }
}

/// 缓存统计结果的持久化文件（应用数据目录）。
fn report_path() -> Result<PathBuf> {
    Ok(config_io::app_dir()?.join("cache-report.json"))
}

/// 把统计结果写入应用缓存；写入失败只影响下次是否复用，不影响界面。
pub(crate) fn save_report(report: &CacheReport) {
    let Ok(path) = report_path() else {
        return;
    };
    let Ok(text) = serde_json::to_string(report) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, text);
}

/// 读取应用缓存中的统计结果；没有缓存、`CARGO_HOME` 变化或缓存目录已更新时返回 `None`。
pub(crate) fn load_report() -> Option<CacheReport> {
    load_report_at(&config_io::cargo_home().ok()?)
}

/// 读取指定 cargo home 的缓存统计结果，仅当指纹与磁盘现状一致时复用。
pub(crate) fn load_report_at(home: &Path) -> Option<CacheReport> {
    let text = fs::read_to_string(report_path().ok()?).ok()?;
    let report: CacheReport = serde_json::from_str(&text).ok()?;
    (report.home == home && report.fingerprint == fingerprint(home)).then_some(report)
}

/// 判定缓存变化时计入指纹的目录层级。
const FINGERPRINT_DEPTH: usize = 3;

/// 缓存目录的变更指纹。
///
/// 只读取各缓存根目录及其近三层子目录的修改时间与条目数量，不做全量遍历：
/// cargo 新增或删除缓存文件都会改变所在目录的修改时间与条目数，因此指纹变化
/// 即表示需要重新统计，指纹不变则可以直接复用上次的结果。
pub(crate) fn fingerprint(home: &Path) -> String {
    let mut stamps = Vec::new();
    for sub in [
        "registry/index",
        "registry/cache",
        "registry/src",
        "git/db",
        "git/checkouts",
    ] {
        collect_stamps(&home.join(sub), 0, &mut stamps);
    }
    stamps.sort();

    // FNV-1a：结果稳定，且无需额外依赖。
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for stamp in stamps {
        for byte in stamp.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{hash:016x}")
}

/// 记录目录的修改时间与条目数，并向下递归到限定层级。
fn collect_stamps(dir: &Path, depth: usize, out: &mut Vec<String>) {
    let Ok(reader) = fs::read_dir(dir) else {
        return;
    };
    let modified = fs::metadata(dir)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|dur| dur.as_nanos())
        .unwrap_or(0);

    let mut count = 0;
    let mut children = Vec::new();
    for entry in reader.flatten() {
        count += 1;
        if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            children.push(entry.path());
        }
    }
    out.push(format!("{}|{modified}|{count}", dir.display()));

    if depth >= FINGERPRINT_DEPTH {
        return;
    }
    for child in children {
        collect_stamps(&child, depth + 1, out);
    }
}

/// 按来源聚合缓存体积：registry 按完整索引目录（即完整索引 URL）分组，git 按仓库目录名分组。
fn collect_sources(home: &Path, index_urls: &[String]) -> Vec<Source> {
    let registry_root = home.join("registry");
    let resolved = resolve_index_urls(&registry_root.join("index"), index_urls);
    let mut registry_sources: BTreeMap<String, Source> = BTreeMap::new();
    for (category, sub) in [
        (Category::RegistryIndex, "index"),
        (Category::RegistryCache, "cache"),
        (Category::RegistrySrc, "src"),
    ] {
        for dir in read_dirs(&registry_root.join(sub)) {
            let Some(dir_name) = file_name(&dir) else {
                continue;
            };
            let (bytes, _) = size_of(&dir);
            let source = registry_sources
                .entry(dir_name.clone())
                .or_insert_with(|| Source {
                    kind: SourceKind::Registry,
                    name: dir_name.clone(),
                    paths: Vec::new(),
                    bytes: 0,
                    parts: Vec::new(),
                    url: resolved.get(&dir_name).cloned(),
                });
            source.bytes += bytes;
            source.paths.push(dir);
            match source
                .parts
                .iter_mut()
                .find(|part| part.category == category)
            {
                Some(part) => part.bytes += bytes,
                None => source.parts.push(SourcePart { category, bytes }),
            }
        }
    }

    let git_root = home.join("git");
    let mut git_sources: BTreeMap<String, Source> = BTreeMap::new();
    for (category, sub) in [
        (Category::GitDb, "db"),
        (Category::GitCheckouts, "checkouts"),
    ] {
        for dir in read_dirs(&git_root.join(sub)) {
            let Some(name) = file_name(&dir) else {
                continue;
            };
            let (bytes, _) = size_of(&dir);
            let url = if category == Category::GitDb {
                git_remote_url(&dir)
            } else {
                None
            };
            let source = git_sources.entry(name.clone()).or_insert_with(|| Source {
                kind: SourceKind::Git,
                name,
                paths: Vec::new(),
                bytes: 0,
                parts: Vec::new(),
                url: None,
            });
            source.bytes += bytes;
            source.paths.push(dir);
            source.parts.push(SourcePart { category, bytes });
            if source.url.is_none() {
                source.url = url;
            }
        }
    }

    let mut sources: Vec<Source> = registry_sources
        .into_values()
        .chain(git_sources.into_values())
        .collect();
    for source in &mut sources {
        source
            .parts
            .sort_by_key(|part| category_rank(part.category));
    }
    sources.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
    sources
}

/// 索引协议：sparse 或 git。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum IndexProtocol {
    Sparse,
    Git,
}

/// 还原 registry 索引目录对应的完整索引 URL。
///
/// cargo 的目录名只保留主机名与 URL 哈希，URL 本身不落盘。这里仅在「同一主机、同一协议
/// 下只有一个目录，且已知配置中也只有一个候选地址」时建立映射，宁可保留目录标识也不
/// 错误合并不同索引。
fn resolve_index_urls(index_root: &Path, urls: &[String]) -> HashMap<String, String> {
    let dirs = read_dirs(index_root);
    let mut dir_counts: HashMap<(String, IndexProtocol), usize> = HashMap::new();
    let mut candidates: HashMap<(String, IndexProtocol), Vec<&str>> = HashMap::new();

    for url in urls {
        if let Some(host) = index_host(url) {
            candidates
                .entry((host, url_protocol(url)))
                .or_default()
                .push(url);
        }
    }
    for dir in &dirs {
        if let Some(host) = file_name(dir).and_then(|name| dir_site(&name)) {
            *dir_counts.entry((host, dir_protocol(dir))).or_default() += 1;
        }
    }

    let mut resolved = HashMap::new();
    for dir in &dirs {
        let Some(name) = file_name(dir) else {
            continue;
        };
        let Some(host) = dir_site(&name) else {
            continue;
        };
        let key = (host, dir_protocol(dir));
        let unique_dir = dir_counts.get(&key) == Some(&1);
        let unique_url = candidates.get(&key).is_some_and(|urls| urls.len() == 1);
        if unique_dir && unique_url {
            resolved.insert(name, candidates[&key][0].to_string());
        }
    }
    resolved
}

/// 收集已知的 registry 索引地址：内置源、自定义源、私有 registry 与 Cargo 配置中的 `[source]`。
fn configured_index_urls() -> Vec<String> {
    let mut urls: Vec<String> = crate::registry::builtins()
        .into_iter()
        .map(|registry| registry.url.to_string())
        .collect();

    if let Ok(customs) = config_io::read_customs() {
        urls.extend(customs.into_values().map(|source| source.url));
    }

    let settings = config_io::read_settings();
    urls.extend(
        config_io::read_private_registries_scoped(settings.project_root.as_deref())
            .into_iter()
            .map(|registry| registry.index),
    );

    let mut paths = vec![config_io::config_path().ok()];
    if let Some(root) = settings.project_root.as_deref() {
        paths.push(Some(config_io::project_config_path(root)));
    }
    for path in paths.into_iter().flatten() {
        let Ok(text) = config_io::read_config_content(&path) else {
            continue;
        };
        let Ok(config) = text.parse::<toml::Table>() else {
            continue;
        };
        let Some(toml::Value::Table(sources)) = config.get("source") else {
            continue;
        };
        for value in sources.values() {
            if let Some(url) = value.get("registry").and_then(toml::Value::as_str) {
                urls.push(url.to_string());
            }
        }
    }

    urls.sort();
    urls.dedup();
    urls
}

/// 从索引 URL 中提取主机名（忽略 `sparse+` 前缀与端口）。
fn index_host(url: &str) -> Option<String> {
    let url = url.strip_prefix("sparse+").unwrap_or(url);
    let rest = url.split_once("://")?.1;
    let host = rest
        .split('/')
        .next()?
        .split(':')
        .next()?
        .to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

/// 索引 URL 的协议类型。
fn url_protocol(url: &str) -> IndexProtocol {
    if url.starts_with("sparse+") {
        IndexProtocol::Sparse
    } else {
        IndexProtocol::Git
    }
}

/// 缓存目录的协议类型：git 索引目录带 `.git`，其余按 sparse 处理。
fn dir_protocol(dir: &Path) -> IndexProtocol {
    if dir.join(".git").exists() {
        IndexProtocol::Git
    } else {
        IndexProtocol::Sparse
    }
}

/// 从 cargo 的 registry 目录名中提取主机：`<host>-<16 位十六进制哈希>`。
fn dir_site(dir_name: &str) -> Option<String> {
    match dir_name.rsplit_once('-') {
        Some((site, hash)) if hash.len() == 16 && hash.chars().all(|ch| ch.is_ascii_hexdigit()) => {
            Some(site.to_ascii_lowercase())
        }
        _ => None,
    }
}

/// 类别在来源明细中的展示顺序。
fn category_rank(category: Category) -> u8 {
    match category {
        Category::RegistryIndex => 0,
        Category::RegistryCache => 1,
        Category::RegistrySrc => 2,
        Category::GitDb => 3,
        Category::GitCheckouts => 4,
    }
}

/// 读取 git 裸仓库的远端地址。
fn git_remote_url(db_dir: &Path) -> Option<String> {
    let config = fs::read_to_string(db_dir.join("config")).ok()?;
    config
        .lines()
        .map(str::trim)
        .find_map(|line| {
            line.strip_prefix("url = ")
                .or_else(|| line.strip_prefix("url="))
        })
        .map(|url| url.trim().to_string())
        .filter(|url| !url.is_empty())
}

/// 清理真实 `CARGO_HOME` 下的无效缓存。
pub(crate) fn clean() -> Result<CleanReport> {
    Ok(clean_at(&config_io::cargo_home()?))
}

/// 删除指定 cargo home 下的无效缓存，正常缓存保持不变。
pub(crate) fn clean_at(home: &Path) -> CleanReport {
    let mut report = CleanReport::default();
    for entry in find_invalid(home) {
        match remove_path(&entry.path) {
            Ok(()) => {
                report.removed += 1;
                report.bytes += entry.bytes;
            }
            Err(_) => report.failed += 1,
        }
    }
    report
}

/// 递归统计目录下普通文件的数量与总字节数；符号链接不计入，读取失败的条目跳过。
pub(crate) fn size_of(path: &Path) -> (u64, u64) {
    let mut bytes = 0;
    let mut files = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(reader) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in reader.flatten() {
            if entry
                .file_type()
                .map(|kind| kind.is_symlink())
                .unwrap_or(true)
            {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                bytes += meta.len();
                files += 1;
            }
        }
    }
    (bytes, files)
}

/// 把字节数格式化为便于阅读的大小。
pub(crate) fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

/// 扫描无效缓存（顺序：先整体目录，后单个条目，避免重复计数）。
fn find_invalid(home: &Path) -> Vec<InvalidEntry> {
    let registry = home.join("registry");
    let index_root = registry.join("index");
    let index_names = dir_names(&index_root);
    let mut invalid = Vec::new();

    // registry 下的缓存 / 源码目录必须有同名索引，否则 cargo 无法再使用。
    for sub in ["cache", "src"] {
        for path in read_dirs(&registry.join(sub)) {
            if !has_name(&index_names, &path) {
                invalid.push(invalid_entry(InvalidKind::OrphanRegistry, path));
            }
        }
    }

    // 解压中断的源码目录：缺少 cargo 写入的完成标记。
    for registry_dir in read_dirs(&registry.join("src")) {
        if !has_name(&index_names, &registry_dir) {
            continue;
        }
        for crate_dir in read_dirs(&registry_dir) {
            if !crate_dir.join(".cargo-ok").is_file() {
                invalid.push(invalid_entry(InvalidKind::IncompleteSrc, crate_dir));
            }
        }
    }

    // git 工作副本必须有对应的裸仓库。
    let db_names = dir_names(&home.join("git").join("db"));
    for path in read_dirs(&home.join("git").join("checkouts")) {
        if !has_name(&db_names, &path) {
            invalid.push(invalid_entry(InvalidKind::OrphanCheckout, path));
        }
    }

    invalid
}

fn invalid_entry(kind: InvalidKind, path: PathBuf) -> InvalidEntry {
    let (bytes, _) = size_of(&path);
    InvalidEntry { kind, path, bytes }
}

/// 目录下全部子目录。
fn read_dirs(path: &Path) -> Vec<PathBuf> {
    let Ok(reader) = fs::read_dir(path) else {
        return Vec::new();
    };
    reader
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect()
}

/// 目录下全部子目录的名称集合。
fn dir_names(path: &Path) -> HashSet<String> {
    read_dirs(path)
        .iter()
        .filter_map(|path| file_name(path))
        .collect()
}

/// 路径的目录名。
fn file_name(path: &Path) -> Option<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
}

/// 路径的目录名是否在集合中。
fn has_name(names: &HashSet<String>, path: &Path) -> bool {
    path.file_name()
        .map(|name| names.contains(name.to_string_lossy().as_ref()))
        .unwrap_or(false)
}

/// 删除文件或目录；遇到只读属性导致失败时先去掉只读位再重试。
fn remove_path(path: &Path) -> std::io::Result<()> {
    fn remove(path: &Path) -> std::io::Result<()> {
        if path.is_dir() {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        }
    }

    match remove(path) {
        Ok(()) => Ok(()),
        Err(_) => {
            make_writable(path);
            remove(path)
        }
    }
}

fn make_writable(path: &Path) {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return;
    };
    if meta.is_dir() {
        if let Ok(children) = fs::read_dir(path) {
            for child in children.flatten() {
                make_writable(&child.path());
            }
        }
    }
    let mut permissions = meta.permissions();
    if permissions.readonly() {
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        let _ = fs::set_permissions(path, permissions);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 每个测试独立的 cargo home 沙箱。
    struct Sandbox {
        home: PathBuf,
    }

    impl Sandbox {
        fn new(label: &str) -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let home = std::env::temp_dir()
                .join(format!("srmgui-cache-{label}-{}-{id}", std::process::id()));
            let _ = fs::remove_dir_all(&home);
            fs::create_dir_all(&home).unwrap();
            Self { home }
        }

        fn write(&self, relative: &str, bytes: usize) {
            let path = self.home.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, vec![0u8; bytes]).unwrap();
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.home);
        }
    }

    fn fixture(label: &str) -> Sandbox {
        let sandbox = Sandbox::new(label);
        // 有效缓存。
        sandbox.write("registry/index/index.crates.io-abc/config.json", 4);
        sandbox.write("registry/cache/index.crates.io-abc/foo-1.0.0.crate", 10);
        sandbox.write("registry/src/index.crates.io-abc/foo-1.0.0/.cargo-ok", 0);
        sandbox.write("registry/src/index.crates.io-abc/foo-1.0.0/src/lib.rs", 6);
        sandbox.write("git/db/gpui-123/config", 3);
        sandbox.write("git/checkouts/gpui-123/abc123/Cargo.toml", 5);
        // 无效缓存。
        sandbox.write("registry/src/index.crates.io-abc/bar-0.1.0/src/lib.rs", 7);
        sandbox.write("registry/cache/gone-xyz/old-1.0.0.crate", 9);
        sandbox.write("registry/src/gone-xyz/old-1.0.0/src/lib.rs", 8);
        sandbox.write("git/checkouts/old-999/def456/Cargo.toml", 4);
        sandbox
    }

    #[test]
    fn scans_registry_and_git_sizes() {
        let sandbox = fixture("sizes");
        let report = scan_at_with_urls(&sandbox.home, &[]);

        assert_eq!(report.registry_bytes, 4 + 10 + 6 + 7 + 9 + 8);
        assert_eq!(report.git_bytes, 3 + 5 + 4);
        assert_eq!(report.total_bytes(), 56);
    }

    #[test]
    fn groups_sizes_by_source() {
        let sandbox = fixture("sources");
        let report = scan_at_with_urls(&sandbox.home, &[]);

        let source = |name: &str| {
            report
                .sources
                .iter()
                .find(|source| source.name == name)
                .unwrap()
        };

        // registry 来源合并 index / cache / src 三部分。
        let crates_io = source("index.crates.io-abc");
        assert_eq!(crates_io.kind, SourceKind::Registry);
        assert_eq!(crates_io.bytes, 4 + 10 + 6 + 7);
        assert_eq!(
            crates_io
                .parts
                .iter()
                .map(|part| (part.category, part.bytes))
                .collect::<Vec<_>>(),
            vec![
                (Category::RegistryIndex, 4),
                (Category::RegistryCache, 10),
                (Category::RegistrySrc, 13),
            ]
        );

        assert_eq!(source("gone-xyz").bytes, 9 + 8);

        // git 来源合并 db / checkouts。
        let gpui = source("gpui-123");
        assert_eq!(gpui.kind, SourceKind::Git);
        assert_eq!(gpui.bytes, 3 + 5);
        assert_eq!(source("old-999").bytes, 4);

        // 按体积从大到小排列。
        assert_eq!(
            report.sources.first().map(|source| source.name.as_str()),
            Some("index.crates.io-abc")
        );
    }

    #[test]
    fn keeps_distinct_indexes_on_same_host_separate() {
        let sandbox = Sandbox::new("indexes");
        // 同一主机下的 sparse 与 git 索引是两个不同的完整索引 URL，不能合并。
        sandbox.write("registry/index/rsproxy.cn-e3de039b2554c837/config.json", 4);
        sandbox.write(
            "registry/cache/rsproxy.cn-e3de039b2554c837/foo-1.0.0.crate",
            10,
        );
        sandbox.write("registry/index/rsproxy.cn-bba5f22c699b99ea/.git/HEAD", 5);
        sandbox.write(
            "registry/cache/rsproxy.cn-bba5f22c699b99ea/bar-1.0.0.crate",
            7,
        );

        let report = scan_at_with_urls(&sandbox.home, &[]);
        assert_eq!(report.sources.len(), 2);

        let sparse = report
            .sources
            .iter()
            .find(|source| source.name == "rsproxy.cn-e3de039b2554c837")
            .unwrap();
        assert_eq!(sparse.bytes, 14);
        let git = report
            .sources
            .iter()
            .find(|source| source.name == "rsproxy.cn-bba5f22c699b99ea")
            .unwrap();
        assert_eq!(git.bytes, 12);
    }

    #[test]
    fn resolves_full_index_url_when_unambiguous() {
        let sandbox = Sandbox::new("resolve");
        sandbox.write("registry/index/rsproxy.cn-e3de039b2554c837/config.json", 4);
        sandbox.write("registry/index/rsproxy.cn-bba5f22c699b99ea/.git/HEAD", 5);

        let urls = vec![
            "sparse+https://rsproxy.cn/index/".to_string(),
            "https://rsproxy.cn/crates.io-index".to_string(),
        ];
        let report = scan_at_with_urls(&sandbox.home, &urls);
        let url = |name: &str| {
            report
                .sources
                .iter()
                .find(|source| source.name == name)
                .unwrap()
                .url
                .clone()
        };

        assert_eq!(
            url("rsproxy.cn-e3de039b2554c837").as_deref(),
            Some("sparse+https://rsproxy.cn/index/")
        );
        assert_eq!(
            url("rsproxy.cn-bba5f22c699b99ea").as_deref(),
            Some("https://rsproxy.cn/crates.io-index")
        );
    }

    #[test]
    fn skips_url_resolution_when_host_has_multiple_indexes() {
        let sandbox = Sandbox::new("ambiguous");
        // 同一主机同协议存在多个目录时无法确定目录与 URL 的对应关系，保留目录标识。
        sandbox.write("registry/index/rsproxy.cn-e3de039b2554c837/config.json", 1);
        sandbox.write("registry/index/rsproxy.cn-bba5f22c699b99ea/config.json", 1);

        let urls = vec!["sparse+https://rsproxy.cn/index/".to_string()];
        let report = scan_at_with_urls(&sandbox.home, &urls);
        assert!(report.sources.iter().all(|source| source.url.is_none()));
    }

    #[test]
    fn extracts_host_from_index_dir_name() {
        assert_eq!(
            dir_site("index.crates.io-1949cf8c6b5b557f").as_deref(),
            Some("index.crates.io")
        );
        assert_eq!(
            dir_site("rsproxy.cn-AAAAAAAAAAAAAAAA").as_deref(),
            Some("rsproxy.cn")
        );
        assert_eq!(dir_site("host-1234"), None);
    }

    #[test]
    fn reads_git_remote_url() {
        let sandbox = Sandbox::new("git-url");
        sandbox.write("git/db/gpui-123/HEAD", 1);
        fs::write(
            sandbox.home.join("git/db/gpui-123/config"),
            "[remote \"origin\"]\n\turl = https://github.com/zed-industries/gpui\n",
        )
        .unwrap();

        let report = scan_at_with_urls(&sandbox.home, &[]);
        let source = report
            .sources
            .iter()
            .find(|source| source.name == "gpui-123")
            .unwrap();
        assert_eq!(
            source.url.as_deref(),
            Some("https://github.com/zed-industries/gpui")
        );
    }

    #[test]
    fn detects_all_invalid_kinds() {
        let sandbox = fixture("invalid");
        let report = scan_at_with_urls(&sandbox.home, &[]);

        let mut kinds: Vec<InvalidKind> = report.invalid.iter().map(|entry| entry.kind).collect();
        kinds.sort_by_key(|kind| format!("{kind:?}"));
        assert_eq!(
            kinds,
            vec![
                InvalidKind::IncompleteSrc,
                InvalidKind::OrphanCheckout,
                InvalidKind::OrphanRegistry,
                InvalidKind::OrphanRegistry,
            ]
        );
        assert_eq!(report.invalid_bytes(), 7 + 9 + 8 + 4);
    }

    #[test]
    fn clean_removes_only_invalid_entries() {
        let sandbox = fixture("clean");
        let report = scan_at_with_urls(&sandbox.home, &[]);
        let invalid_bytes = report.invalid_bytes();

        let cleaned = clean_at(&sandbox.home);
        assert_eq!(cleaned.removed, 4);
        assert_eq!(cleaned.failed, 0);
        assert_eq!(cleaned.bytes, invalid_bytes);

        // 有效缓存全部保留。
        for relative in [
            "registry/index/index.crates.io-abc/config.json",
            "registry/cache/index.crates.io-abc/foo-1.0.0.crate",
            "registry/src/index.crates.io-abc/foo-1.0.0/.cargo-ok",
            "registry/src/index.crates.io-abc/foo-1.0.0/src/lib.rs",
            "git/db/gpui-123/config",
            "git/checkouts/gpui-123/abc123/Cargo.toml",
        ] {
            assert!(sandbox.home.join(relative).is_file(), "{relative} 被误删");
        }
        assert!(scan_at_with_urls(&sandbox.home, &[]).invalid.is_empty());
    }

    #[test]
    fn missing_cache_dirs_produce_empty_report() {
        let sandbox = Sandbox::new("empty");
        let report = scan_at_with_urls(&sandbox.home, &[]);
        assert!(report.sources.is_empty());
        assert!(report.invalid.is_empty());
        assert_eq!(report.total_bytes(), 0);
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1536), "1.50 KB");
        assert_eq!(format_size(3 * 1024 * 1024), "3.00 MB");
    }

    #[test]
    fn fingerprint_changes_only_when_cache_updates() {
        let sandbox = fixture("fingerprint");
        let before = scan_at_with_urls(&sandbox.home, &[]);
        // 未改动时两次统计的指纹一致，可复用结果。
        assert_eq!(
            before.fingerprint,
            scan_at_with_urls(&sandbox.home, &[]).fingerprint
        );

        sandbox.write("registry/cache/index.crates.io-abc/new-1.0.0.crate", 6);
        assert_ne!(before.fingerprint, fingerprint(&sandbox.home));
    }

    #[test]
    fn reuses_saved_report_until_cache_changes() {
        let sandbox = fixture("persist");
        config_io::set_test_app_dir(Some(sandbox.home.join("app")));

        let report = scan_at_with_urls(&sandbox.home, &[]);
        save_report(&report);
        let loaded = load_report_at(&sandbox.home).expect("缓存结果应可直接复用");
        assert_eq!(loaded.total_bytes(), report.total_bytes());
        assert_eq!(loaded.sources.len(), report.sources.len());

        // 缓存有更新后旧结果失效。
        sandbox.write("git/db/gpui-123/new", 1);
        assert!(load_report_at(&sandbox.home).is_none());

        config_io::set_test_app_dir(None);
    }

    #[test]
    fn ignores_report_from_other_cargo_home() {
        let sandbox = fixture("other-home");
        config_io::set_test_app_dir(Some(sandbox.home.join("app")));

        save_report(&scan_at_with_urls(&sandbox.home, &[]));
        assert!(load_report_at(&sandbox.home.join("another")).is_none());

        config_io::set_test_app_dir(None);
    }
}
