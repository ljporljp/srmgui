//! cargo 配置健康检查：识别被其他工具改坏后残留的问题，并支持一键修复。
//!
//! 检查项：
//! - 重复的表头（例如其他工具反复追加 `[source.crates-io]`）
//! - `replace-with` 指向不存在的源（残留替换块）
//! - 未被引用且不受 srmgui 管理的 `[source.x]` 段（孤立源段）
//! - 无法解析的 TOML 语法错误（修复时回退到最近的有效备份）

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use toml_edit::{DocumentMut, Item};

use crate::config_io;

/// 问题类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IssueKind {
    /// 同一个表头出现多次，TOML 无法解析。
    DuplicateTable,
    /// `replace-with` 指向的源在配置中不存在。
    DanglingReplace,
    /// 未被引用且不由 srmgui 管理的 `[source.x]` 段。
    OrphanSource,
    /// 无法解析的 TOML 语法错误。
    Syntax,
}

/// 一条检测到的问题。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Issue {
    pub kind: IssueKind,
    /// 表名、源名或语法错误详情。
    pub detail: String,
}

/// 一次健康检查的结果。
#[derive(Debug, Clone)]
pub(crate) struct HealthReport {
    /// 被检查的配置文件。
    pub path: PathBuf,
    /// 作用域：`global` 或 `project`。
    pub scope: &'static str,
    /// 配置文件是否存在。
    pub exists: bool,
    /// 检测到的问题（为空表示健康）。
    pub issues: Vec<Issue>,
}

impl HealthReport {
    /// 是否没有检测到任何问题。
    pub(crate) fn is_healthy(&self) -> bool {
        self.issues.is_empty()
    }
}

/// 一次修复的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FixSummary {
    /// 被处理的问题数量。
    pub fixed: usize,
    /// 是否通过恢复快照 / `.bak` 备份完成修复。
    pub restored: bool,
    /// 配置无法解析且没有可用备份，未能修复。
    pub unfixable: bool,
}

impl FixSummary {
    fn nothing() -> Self {
        Self {
            fixed: 0,
            restored: false,
            unfixable: false,
        }
    }
}

/// 扫描指定配置文件，返回检测结果。
pub(crate) fn scan(path: &Path, scope: &'static str) -> Result<HealthReport> {
    let content = config_io::read_config_content(path)?;
    let exists = path.is_file();
    let issues = if exists {
        inspect(&content, &managed_source_ids())
    } else {
        Vec::new()
    };
    Ok(HealthReport {
        path: path.to_path_buf(),
        scope,
        exists,
        issues,
    })
}

/// 修复检测到的问题：优先按语义修补，语法错误则回退到最近的有效备份。
pub(crate) fn fix(path: &Path, scope: &str) -> Result<FixSummary> {
    if !path.is_file() {
        return Ok(FixSummary::nothing());
    }
    let content = config_io::read_config_content(path)?;
    let managed = managed_source_ids();
    let issues = inspect(&content, &managed);
    if issues.is_empty() {
        return Ok(FixSummary::nothing());
    }

    match repair(&content, &managed) {
        Ok(Some(fixed)) => {
            config_io::write_repaired(path, scope, &fixed)?;
            Ok(FixSummary {
                fixed: issues.len(),
                restored: false,
                unfixable: false,
            })
        }
        Ok(None) => Ok(FixSummary::nothing()),
        // 无法自动修补的语法错误：回到最近一份能解析的备份。
        Err(_) => {
            if let Some(snapshot) = latest_valid_snapshot(path) {
                config_io::restore_snapshot(&snapshot)?;
                return Ok(FixSummary {
                    fixed: issues.len(),
                    restored: true,
                    unfixable: false,
                });
            }
            if let Some(backup) = read_valid_backup(path) {
                let snapshot = config_io::Snapshot {
                    created_at: crate::logger::format_timestamp(),
                    target: path.to_path_buf(),
                    scope: scope.to_string(),
                    source: "official".to_string(),
                    content: backup,
                };
                config_io::restore_snapshot(&snapshot)?;
                return Ok(FixSummary {
                    fixed: issues.len(),
                    restored: true,
                    unfixable: false,
                });
            }
            Ok(FixSummary {
                fixed: 0,
                restored: false,
                unfixable: true,
            })
        }
    }
}

/// srmgui 认为合法的源标识：全部内置源 + 元数据中登记的自定义源。
fn managed_source_ids() -> HashSet<String> {
    let mut ids: HashSet<String> = crate::manifest::current_sources()
        .into_iter()
        .map(|source| source.id)
        .collect();
    ids.extend(config_io::custom_metadata_ids());
    ids
}

/// 解析配置内容并列出全部问题（纯函数，便于测试）。
fn inspect(content: &str, managed: &HashSet<String>) -> Vec<Issue> {
    if content.trim().is_empty() {
        return Vec::new();
    }

    let mut issues = Vec::new();
    let mut duplicates = duplicate_tables(content);
    duplicates.sort();
    for table in duplicates {
        issues.push(Issue {
            kind: IssueKind::DuplicateTable,
            detail: table,
        });
    }

    // 重复表头本身会让解析失败，先按「保留最后一段」消除，
    // 否则它会把后面真正需要检查的内容全部掩盖掉。
    let deduped = dedupe_tables(content);
    let Ok(doc) = deduped.parse::<DocumentMut>() else {
        issues.push(Issue {
            kind: IssueKind::Syntax,
            detail: parse_error(&deduped),
        });
        return issues;
    };

    let replaced = replace_targets(&doc);
    for target in &replaced {
        if !source_names(&doc).iter().any(|name| name == target) {
            issues.push(Issue {
                kind: IssueKind::DanglingReplace,
                detail: target.clone(),
            });
        }
    }

    for name in source_names(&doc) {
        if name == "crates-io" {
            continue;
        }
        // 正在生效的替换目标不能删。
        if replaced.iter().any(|target| target == &name) {
            continue;
        }
        // 内置源与 srmgui 元数据中登记的自定义源都可能在切换后暂时闲置。
        if managed.contains(&name) {
            continue;
        }
        issues.push(Issue {
            kind: IssueKind::OrphanSource,
            detail: name,
        });
    }

    issues
}

/// 生成修复后的完整内容；无法解析时返回错误（由调用方回退到备份）。
fn repair(content: &str, managed: &HashSet<String>) -> Result<Option<String>> {
    let deduped = dedupe_tables(content);
    let mut doc = deduped
        .parse::<DocumentMut>()
        .with_context(|| rust_i18n::t!("error.parse_cargo_config").to_string())?;
    let mut changed = deduped != content;

    // 清掉指向不存在源的 replace-with，恢复默认行为。
    let defined = source_names(&doc);
    let dangling_sources: Vec<String> = replace_targets(&doc)
        .into_iter()
        .filter(|target| !defined.iter().any(|name| name == target))
        .collect();
    if !dangling_sources.is_empty() {
        let names = source_names(&doc);
        if let Some(sources) = doc.get_mut("source").and_then(Item::as_table_like_mut) {
            for name in names {
                let points_to_missing = sources
                    .get(&name)
                    .and_then(Item::as_table_like)
                    .and_then(|table| table.get("replace-with"))
                    .and_then(Item::as_str)
                    .is_some_and(|target| dangling_sources.iter().any(|missing| missing == target));
                if points_to_missing {
                    if let Some(table) = sources.get_mut(&name).and_then(Item::as_table_like_mut) {
                        table.remove("replace-with");
                        changed = true;
                    }
                }
            }
        }
    }

    // 删除不再被引用、也不由 srmgui 管理的源段。
    let replaced = replace_targets(&doc);
    let orphans: Vec<String> = source_names(&doc)
        .into_iter()
        .filter(|name| name != "crates-io")
        .filter(|name| !replaced.iter().any(|target| target == name))
        .filter(|name| !managed.contains(name))
        .collect();
    if !orphans.is_empty() {
        if let Some(sources) = doc.get_mut("source").and_then(Item::as_table_like_mut) {
            for name in orphans {
                sources.remove(&name);
                changed = true;
            }
        }
    }

    Ok(if changed { Some(doc.to_string()) } else { None })
}

/// 配置中出现的全部表头（行号 + 表名），忽略跨行字符串中的伪表头。
fn scan_headers(content: &str) -> Vec<(usize, String)> {
    let mut headers = Vec::new();
    let mut multiline: Option<&'static str> = None;
    for (index, line) in content.lines().enumerate() {
        let inside = multiline.is_some();
        if !inside {
            if let Some(name) = table_name(line) {
                headers.push((index, name));
            }
        }
        toggle_multiline(line, &mut multiline);
    }
    headers
}

/// 从一行中解析表名；`[[array]]`、内联表与注释行返回 `None`。
fn table_name(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.starts_with("[[") {
        return None;
    }
    let rest = trimmed.strip_prefix('[')?;
    let end = rest.find(']')?;
    let name = rest[..end].trim();
    if name.is_empty() {
        return None;
    }
    Some(name.to_string())
}

/// 跟踪 `"""` / `'''` 跨行字符串状态，避免把字符串内容误判为表头。
fn toggle_multiline(line: &str, state: &mut Option<&'static str>) {
    let mut rest = line;
    loop {
        match *state {
            None => {
                let basic = rest.find("\"\"\"");
                let literal = rest.find("'''");
                match (basic, literal) {
                    (Some(b), Some(l)) if b < l => {
                        *state = Some("\"\"\"");
                        rest = &rest[b + 3..];
                    }
                    (Some(b), None) => {
                        *state = Some("\"\"\"");
                        rest = &rest[b + 3..];
                    }
                    (_, Some(l)) => {
                        *state = Some("'''");
                        rest = &rest[l + 3..];
                    }
                    (None, None) => break,
                }
            }
            Some(quote) => {
                if let Some(pos) = rest.find(quote) {
                    *state = None;
                    rest = &rest[pos + 3..];
                } else {
                    break;
                }
            }
        }
    }
}

/// 出现多次的表头名称。
fn duplicate_tables(content: &str) -> Vec<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for (_, name) in scan_headers(content) {
        *counts.entry(name).or_insert(0) += 1;
    }
    counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(name, _)| name)
        .collect()
}

/// 消除重复表头：同一表名只保留最后一段，其余段整体删除。
fn dedupe_tables(content: &str) -> String {
    let headers = scan_headers(content);
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for (_, name) in &headers {
        *counts.entry(name.as_str()).or_insert(0) += 1;
    }
    if counts.values().all(|count| *count <= 1) {
        return content.to_string();
    }

    let lines: Vec<&str> = content.lines().collect();
    let mut remove = vec![false; lines.len()];
    for (index, (line, name)) in headers.iter().enumerate() {
        if counts[name.as_str()] <= 1 {
            continue;
        }
        // 只保留最后一次出现。
        let is_last = !headers[index + 1..].iter().any(|(_, later)| later == name);
        if is_last {
            continue;
        }
        let end = headers
            .get(index + 1)
            .map(|(next, _)| *next)
            .unwrap_or(lines.len());
        for flag in &mut remove[*line..end] {
            *flag = true;
        }
    }

    let mut out = String::new();
    for (line, dropped) in lines.iter().zip(remove) {
        if !dropped {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// `[source]` 下定义的全部源标识。
fn source_names(doc: &DocumentMut) -> Vec<String> {
    doc.get("source")
        .and_then(Item::as_table_like)
        .map(|sources| sources.iter().map(|(name, _)| name.to_string()).collect())
        .unwrap_or_default()
}

/// 所有 `[source.x]` 的 `replace-with` 目标。
fn replace_targets(doc: &DocumentMut) -> Vec<String> {
    let Some(sources) = doc.get("source").and_then(Item::as_table_like) else {
        return Vec::new();
    };
    sources
        .iter()
        .filter_map(|(_, item)| {
            item.as_table_like()?
                .get("replace-with")?
                .as_str()
                .map(str::to_string)
        })
        .collect()
}

/// 取 TOML 解析错误的首行，避免把多行代码片段塞进界面。
fn parse_error(content: &str) -> String {
    match content.parse::<toml::Table>() {
        Ok(_) => String::new(),
        Err(error) => error
            .to_string()
            .lines()
            .next()
            .unwrap_or_default()
            .to_string(),
    }
}

/// 最近一份目标明确且内容可解析的快照（`read_snapshots` 已按时间从新到旧排序）。
fn latest_valid_snapshot(path: &Path) -> Option<config_io::Snapshot> {
    config_io::read_snapshots()
        .into_iter()
        .find(|snapshot| snapshot.target == path && snapshot.content.parse::<toml::Table>().is_ok())
}

/// 读取 `<配置文件>.bak`，内容可解析时才返回。
fn read_valid_backup(path: &Path) -> Option<String> {
    let mut backup = path.as_os_str().to_os_string();
    backup.push(".bak");
    let text = std::fs::read_to_string(PathBuf::from(backup)).ok()?;
    text.parse::<toml::Table>().ok()?;
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn managed(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    #[test]
    fn healthy_config_has_no_issues() {
        let content = "[source.crates-io]\nreplace-with = \"rsproxy\"\n\n\
                       [source.rsproxy]\nregistry = \"sparse+https://rsproxy.cn/index/\"\n\n\
                       [net]\ngit-fetch-with-cli = true\n";
        assert!(inspect(content, &managed(&["rsproxy"])).is_empty());
    }

    #[test]
    fn detects_and_repairs_dangling_replacement() {
        let content = "[source.crates-io]\nreplace-with = \"gone\"\n";
        let issues = inspect(content, &managed(&[]));
        assert_eq!(
            issues,
            vec![Issue {
                kind: IssueKind::DanglingReplace,
                detail: "gone".to_string(),
            }]
        );

        let repaired = repair(content, &managed(&[])).unwrap().unwrap();
        let doc: toml::Table = repaired.parse().unwrap();
        assert!(
            doc["source"]["crates-io"]
                .as_table()
                .is_none_or(|table| !table.contains_key("replace-with"))
        );
    }

    #[test]
    fn detects_and_repairs_orphan_sources() {
        let content = "[source.crates-io]\n\n\
                       [source.leftover]\nregistry = \"sparse+https://leftover.example/index/\"\n";
        let issues = inspect(content, &managed(&[]));
        assert_eq!(
            issues,
            vec![Issue {
                kind: IssueKind::OrphanSource,
                detail: "leftover".to_string(),
            }]
        );

        let repaired = repair(content, &managed(&[])).unwrap().unwrap();
        assert!(!repaired.contains("source.leftover"));
    }

    #[test]
    fn keeps_managed_and_referenced_sources() {
        let content = "[source.crates-io]\nreplace-with = \"corp\"\n\n\
                       [source.corp]\nregistry = \"sparse+https://corp.example/index/\"\n\n\
                       [source.idle]\nregistry = \"sparse+https://idle.example/index/\"\n";
        // corp 正在生效，idle 在 srmgui 元数据中登记，二者都不算孤立。
        assert!(inspect(content, &managed(&["idle"])).is_empty());
        assert_eq!(repair(content, &managed(&["idle"])).unwrap(), None);
    }

    #[test]
    fn detects_duplicate_headers_and_keeps_last_block() {
        let content = "[source.crates-io]\nreplace-with = \"a\"\n\n\
                       [source.a]\nregistry = \"sparse+https://a.example/index/\"\n\n\
                       [source.crates-io]\nreplace-with = \"b\"\n\n\
                       [source.b]\nregistry = \"sparse+https://b.example/index/\"\n";
        let issues = inspect(content, &managed(&[]));
        assert!(issues.iter().any(|issue| {
            issue.kind == IssueKind::DuplicateTable && issue.detail == "source.crates-io"
        }));

        let repaired = repair(content, &managed(&[])).unwrap().unwrap();
        let doc: toml::Table = repaired.parse().unwrap();
        assert_eq!(
            doc["source"]["crates-io"]["replace-with"].as_str(),
            Some("b")
        );
        // a 段不再被引用，作为孤立源删除；b 段仍在使用。
        assert!(doc["source"].as_table().unwrap().get("a").is_none());
        assert!(doc["source"].as_table().unwrap().get("b").is_some());
    }

    #[test]
    fn dedupe_preserves_other_content() {
        let content = "# 用户注释\n[net]\ngit-fetch-with-cli = true\n\n\
                       [source.crates-io]\nreplace-with = \"a\"\n\n\
                       [source.crates-io]\nreplace-with = \"b\"\n";
        let out = dedupe_tables(content);
        assert!(out.contains("# 用户注释"));
        assert!(out.contains("git-fetch-with-cli = true"));
        assert!(!out.contains("replace-with = \"a\""));
        assert!(out.contains("replace-with = \"b\""));
    }

    #[test]
    fn scanner_skips_multiline_string_content() {
        let content = "[package]\ndescription = \"\"\"\n[source.fake]\n\"\"\"\n\n\
                       [source.crates-io]\nreplace-with = \"x\"\n";
        let names: Vec<String> = scan_headers(content)
            .into_iter()
            .map(|(_, name)| name)
            .collect();
        assert_eq!(
            names,
            vec!["package".to_string(), "source.crates-io".to_string()]
        );
    }

    #[test]
    fn reports_unparseable_content() {
        let issues = inspect("not = [valid\n", &managed(&[]));
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind, IssueKind::Syntax);
        assert!(issues[0].detail.contains("line 1"));
    }
}

/// 端到端集成测试：真实读写临时目录中的配置文件、快照与 `.bak`。
#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::config_io::{self, SCOPE_GLOBAL};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 每个测试独立的临时沙箱，同时把 srmgui 应用目录（快照 / 元数据）重定向进去。
    struct Sandbox {
        root: PathBuf,
        app: PathBuf,
    }

    impl Sandbox {
        fn new(label: &str) -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir()
                .join(format!("srmgui-health-{label}-{}-{id}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            let app = root.join("app");
            fs::create_dir_all(&app).unwrap();
            // 按线程重定向，避免污染真实用户目录与并行测试。
            config_io::set_test_app_dir(Some(app.clone()));
            fs::write(app.join("srmgui-customs.toml"), "").unwrap();
            Self { root, app }
        }

        fn config(&self) -> PathBuf {
            self.root.join("config.toml")
        }

        fn backup(&self) -> PathBuf {
            let mut path = self.config().as_os_str().to_os_string();
            path.push(".bak");
            PathBuf::from(path)
        }

        fn write_metadata(&self, content: &str) {
            fs::write(self.app.join("srmgui-customs.toml"), content).unwrap();
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            config_io::set_test_app_dir(None);
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn scan_then_fix_repairs_all_issue_kinds() {
        let sandbox = Sandbox::new("repair-all");
        let config = sandbox.config();
        let broken = "[net]\ngit-fetch-with-cli = true\n\n\
                      [source.crates-io]\nreplace-with = \"old\"\n\n\
                      [source.crates-io]\nreplace-with = \"ghost\"\n\n\
                      [source.leftover]\nregistry = \"sparse+https://leftover.example/index/\"\n";
        fs::write(&config, broken).unwrap();

        let report = scan(&config, SCOPE_GLOBAL).unwrap();
        assert!(report.exists);
        assert_eq!(report.issues.len(), 3);
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.kind == IssueKind::DuplicateTable)
        );
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.kind == IssueKind::DanglingReplace)
        );
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.kind == IssueKind::OrphanSource)
        );

        let summary = fix(&config, SCOPE_GLOBAL).unwrap();
        assert_eq!(
            summary,
            FixSummary {
                fixed: 3,
                restored: false,
                unfixable: false,
            }
        );

        let repaired = fs::read_to_string(&config).unwrap();
        assert!(repaired.contains("git-fetch-with-cli = true"));
        assert!(!repaired.contains("replace-with"));
        assert!(!repaired.contains("source.leftover"));
        // 修复前的内容进入 `.bak`，并保留了一份快照。
        assert_eq!(fs::read_to_string(sandbox.backup()).unwrap(), broken);
        assert!(
            config_io::read_snapshots()
                .iter()
                .any(|snapshot| snapshot.target == config && snapshot.content == broken)
        );
        // 修复后复检应为健康状态。
        assert!(scan(&config, SCOPE_GLOBAL).unwrap().is_healthy());
    }

    #[test]
    fn scan_respects_custom_metadata_and_leaves_healthy_config_untouched() {
        let sandbox = Sandbox::new("healthy");
        sandbox.write_metadata(
            "[[custom]]\nid = \"idle\"\nurl = \"sparse+https://idle.example/index/\"\n",
        );
        let config = sandbox.config();
        let content = "[source.crates-io]\nreplace-with = \"corp\"\n\n\
                       [source.corp]\nregistry = \"sparse+https://corp.example/index/\"\n\n\
                       [source.idle]\nregistry = \"sparse+https://idle.example/index/\"\n";
        fs::write(&config, content).unwrap();

        // corp 正在生效，idle 在 srmgui 元数据中登记，均不属于孤立源。
        assert!(scan(&config, SCOPE_GLOBAL).unwrap().is_healthy());

        let summary = fix(&config, SCOPE_GLOBAL).unwrap();
        assert_eq!(
            summary,
            FixSummary {
                fixed: 0,
                restored: false,
                unfixable: false,
            }
        );
        assert_eq!(fs::read_to_string(&config).unwrap(), content);
        assert!(!sandbox.backup().exists());
    }

    #[test]
    fn fix_restores_from_snapshot_when_syntax_is_broken() {
        let sandbox = Sandbox::new("restore-snapshot");
        let config = sandbox.config();
        let broken = "[net]\ngit-fetch-with-cli = true\nnot = [valid\n";
        let valid = "[source.crates-io]\nreplace-with = \"rsproxy\"\n\n\
                     [source.rsproxy]\nregistry = \"sparse+https://rsproxy.cn/index/\"\n";
        fs::write(&config, broken).unwrap();
        config_io::save_snapshot(&config, SCOPE_GLOBAL, "", valid).unwrap();

        let report = scan(&config, SCOPE_GLOBAL).unwrap();
        assert_eq!(report.issues.len(), 1);
        assert_eq!(report.issues[0].kind, IssueKind::Syntax);

        let summary = fix(&config, SCOPE_GLOBAL).unwrap();
        assert_eq!(
            summary,
            FixSummary {
                fixed: 1,
                restored: true,
                unfixable: false,
            }
        );
        assert_eq!(fs::read_to_string(&config).unwrap(), valid);
        // 被替换下来的损坏文件保留在 `.bak`。
        assert_eq!(fs::read_to_string(sandbox.backup()).unwrap(), broken);
    }

    #[test]
    fn fix_restores_from_bak_when_no_snapshot_exists() {
        let sandbox = Sandbox::new("restore-bak");
        let config = sandbox.config();
        let valid = "[net]\ngit-fetch-with-cli = true\n";
        fs::write(&config, "not = [valid\n").unwrap();
        fs::write(sandbox.backup(), valid).unwrap();

        let summary = fix(&config, SCOPE_GLOBAL).unwrap();
        assert!(summary.restored);
        assert!(!summary.unfixable);
        assert_eq!(fs::read_to_string(&config).unwrap(), valid);
    }

    #[test]
    fn missing_config_is_healthy_and_nothing_to_fix() {
        let sandbox = Sandbox::new("missing");
        let config = sandbox.config();

        let report = scan(&config, SCOPE_GLOBAL).unwrap();
        assert!(!report.exists);
        assert!(report.is_healthy());

        assert_eq!(
            fix(&config, SCOPE_GLOBAL).unwrap(),
            FixSummary {
                fixed: 0,
                restored: false,
                unfixable: false,
            }
        );
        assert!(!config.exists());
    }
}
