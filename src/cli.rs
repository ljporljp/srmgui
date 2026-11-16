//! 命令行子命令（`srmgui status`、`srmgui use rsproxy` 等）。
//!
//! 与 GUI 共用 [`crate::config_io`] 的配置读写逻辑：同一个作用域、同一个
//! `config.toml`、同一份快照与 `.bak` 备份，因此脚本 / CI 中的切换
//! 与界面上的切换完全等价。
//!
//! 输出固定使用英文且不受界面语言影响，便于脚本解析；错误写入 stderr。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::json;

use crate::config_io::{self, CustomSource, SCOPE_GLOBAL, SCOPE_PROJECT};
use crate::health;
use crate::logger;
use crate::manifest::{self, ManifestSource};
use crate::net;
use crate::registry;
use crate::util;

const USAGE: &str = "\
srmgui - Cargo registry mirror switcher

USAGE:
    srmgui [--json] [--scope <global|project>] [--project <dir>] <COMMAND> [ARGS]
    srmgui                      Open the GUI

COMMANDS:
    status                     Show scope, config file and the active source
    list                       List built-in and custom sources
    use <id>                   Switch Cargo to the given source
    probe [<id>...]            Test connectivity and speed (default: active source)
    custom list                List custom sources
    custom add <id> --url <u>  Add a custom source [--name <n>] [--description <d>]
    custom remove <id>         Remove a custom source
    custom export <path>       Export custom sources to a .srmgui file
    custom import <path>       Import custom sources from a .srmgui file
    registry list              List private registries
    registry add <name> --index <url> [--token <t>]
                               Add or update a private registry
    registry remove <name>     Remove a private registry
    snapshot list              List configuration snapshots
    snapshot restore <n>       Restore snapshot <n> (1-based) or \"latest\"
    scope [global|project]     Show or change the default scope (shared with the GUI)
    doctor [--fix]             Detect and optionally repair config problems
    gui                        Open the GUI
    help                       Show this help

OPTIONS:
    --json                     Print machine-readable JSON
    --scope <global|project>   Override the scope for this command
    --project <dir>            Project directory (any path inside the project)
    --all                      probe: test every known source
    -h, --help                 Show this help
    -V, --version              Show the version

EXIT CODES:
    0 success, 1 failure (or probe/doctor problems), 2 usage error
";

/// 解析后的命令行参数：位置参数（子命令与参数）加全局选项。
#[derive(Debug, Default)]
struct Args {
    words: Vec<String>,
    json: bool,
    all: bool,
    fix: bool,
    /// `Some(true)` 表示项目作用域，`Some(false)` 表示全局作用域。
    scope: Option<bool>,
    project: Option<PathBuf>,
    url: Option<String>,
    name: Option<String>,
    description: Option<String>,
    token: Option<String>,
    index: Option<String>,
    help: bool,
    version: bool,
}

/// 解析要操作的配置文件位置。
struct Target {
    project: bool,
    root: Option<PathBuf>,
}

impl Target {
    fn scope(&self) -> &'static str {
        if self.project {
            SCOPE_PROJECT
        } else {
            SCOPE_GLOBAL
        }
    }

    /// 项目作用域下的项目根目录，全局作用域为 `None`。
    fn root(&self) -> Option<&Path> {
        if self.project {
            self.root.as_deref()
        } else {
            None
        }
    }

    fn config(&self) -> Result<PathBuf> {
        Ok(config_io::config_path_scoped(self.root())?)
    }
}

/// 入口：无参数或 `gui` 子命令时返回 `None`，由调用方启动 GUI。
pub(crate) fn run() -> Option<i32> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.is_empty() {
        return None;
    }

    let args = match parse_args(raw) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("error: {error:#}");
            return Some(2);
        }
    };
    if args.help {
        println!("{USAGE}");
        return Some(0);
    }
    if args.version {
        println!("srmgui {}", env!("CARGO_PKG_VERSION"));
        return Some(0);
    }
    if args.words.first().is_some_and(|word| word == "gui") {
        return None;
    }

    // 共享逻辑里的提示文案由 i18n 提供，CLI 固定用英文输出。
    rust_i18n::set_locale("en-US");

    Some(match dispatch(&args) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error:#}");
            if error.downcast_ref::<UsageError>().is_some() {
                2
            } else {
                1
            }
        }
    })
}

/// 用法错误：参数缺失或子命令不存在，退出码固定为 2。
#[derive(Debug)]
struct UsageError(String);

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UsageError {}

fn usage(message: impl Into<String>) -> anyhow::Error {
    UsageError(message.into()).into()
}

fn dispatch(args: &Args) -> Result<i32> {
    match args.words.first().map(String::as_str) {
        Some("status") => cmd_status(args),
        Some("list") | Some("ls") => cmd_list(args),
        Some("use") => cmd_use(args),
        Some("probe") | Some("test") => cmd_probe(args),
        Some("custom") => cmd_custom(args),
        Some("registry") | Some("registries") => cmd_registry(args),
        Some("snapshot") | Some("snapshots") => cmd_snapshot(args),
        Some("scope") => cmd_scope(args),
        Some("doctor") => cmd_doctor(args),
        Some(other) => Err(usage(format!(
            "unknown command: {other} (run `srmgui --help` for usage)"
        ))),
        None => {
            println!("{USAGE}");
            Ok(0)
        }
    }
}

fn parse_args(raw: Vec<String>) -> Result<Args> {
    let mut args = Args::default();
    let mut iter = raw.into_iter().peekable();

    while let Some(arg) = iter.next() {
        if arg == "-h" || arg == "--help" {
            args.help = true;
            continue;
        }
        if arg == "-V" || arg == "--version" {
            args.version = true;
            continue;
        }
        let Some(rest) = arg.strip_prefix("--") else {
            if arg.starts_with('-') && arg != "-" {
                bail!("unknown option: {arg}");
            }
            args.words.push(arg);
            continue;
        };

        let (key, inline) = match rest.split_once('=') {
            Some((key, value)) => (key, Some(value.to_string())),
            None => (rest, None),
        };
        let mut take = |flag: &str| -> Result<String> {
            inline
                .clone()
                .or_else(|| iter.next())
                .with_context(|| format!("--{flag} requires a value"))
        };

        match key {
            "json" => args.json = true,
            "all" => args.all = true,
            "fix" => args.fix = true,
            "scope" => {
                let value = take("scope")?;
                args.scope = Some(match value.as_str() {
                    "global" => false,
                    "project" => true,
                    other => bail!("--scope expects \"global\" or \"project\", got \"{other}\""),
                });
            }
            "project" => args.project = Some(PathBuf::from(take("project")?)),
            "url" => args.url = Some(take("url")?),
            "name" => args.name = Some(take("name")?),
            "description" => args.description = Some(take("description")?),
            "token" => args.token = Some(take("token")?),
            "index" => args.index = Some(take("index")?),
            other => bail!("unknown option: --{other}"),
        }
    }

    Ok(args)
}

/// 解析本次命令要操作的作用域与配置路径。
///
/// 优先级：命令行 `--scope` / `--project` > GUI 持久化的设置 > 当前目录向上识别。
fn resolve_target(args: &Args) -> Result<Target> {
    let settings = config_io::read_settings();
    let remembered = settings
        .project_root
        .filter(|root| root.join("Cargo.toml").is_file());

    let project = match args.scope {
        Some(flag) => flag,
        None => args.project.is_some() || (settings.project_scope && remembered.is_some()),
    };
    if !project {
        return Ok(Target {
            project: false,
            root: None,
        });
    }

    let root = match &args.project {
        Some(dir) => config_io::detect_project_root(dir).with_context(|| {
            format!(
                "no Cargo.toml found in {} or any parent directory",
                dir.display()
            )
        })?,
        None => remembered
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .and_then(|dir| config_io::detect_project_root(&dir))
            })
            .context("project scope needs a Cargo project; pass --project <dir>")?,
    };
    Ok(Target {
        project: true,
        root: Some(root),
    })
}

/// 当前可用的内置源清单（缓存 → 编译期兜底，不访问网络）。
fn builtins() -> Vec<ManifestSource> {
    manifest::current_sources()
}

fn builtin_ids(builtins: &[ManifestSource]) -> HashSet<String> {
    builtins.iter().map(|source| source.id.clone()).collect()
}

/// 自定义源：srmgui 元数据 + 当前作用域 cargo 配置里出现的额外源。
fn load_customs(
    target: &Target,
    builtins: &[ManifestSource],
) -> Result<HashMap<String, CustomSource>> {
    let ids = builtin_ids(builtins);
    let mut customs = config_io::read_customs()?;
    customs.retain(|id, _| !ids.contains(id.as_str()));

    if let Ok(path) = target.config() {
        for (id, url) in config_sources(&path) {
            if ids.contains(&id) || customs.contains_key(&id) {
                continue;
            }
            customs.insert(id.clone(), CustomSource::new(id, url, ""));
        }
    }
    Ok(customs)
}

/// 读取 cargo 配置文件里 `[source.<id>] registry` 条目。
fn config_sources(path: &Path) -> Vec<(String, String)> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(root) = text.parse::<toml::Table>() else {
        return Vec::new();
    };
    root.get("source")
        .and_then(toml::Value::as_table)
        .map(|sources| {
            sources
                .iter()
                .filter(|(id, _)| id.as_str() != "crates-io")
                .filter_map(|(id, value)| {
                    let url = value.as_table()?.get("registry")?.as_str()?;
                    Some((id.clone(), url.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn source_url(
    id: &str,
    builtins: &[ManifestSource],
    customs: &HashMap<String, CustomSource>,
) -> Option<String> {
    builtins
        .iter()
        .find(|source| source.id == id)
        .map(|source| source.url.clone())
        .or_else(|| customs.get(id).map(|source| source.url.clone()))
}

fn cmd_status(args: &Args) -> Result<i32> {
    let target = resolve_target(args)?;
    let path = target.config()?;
    let exists = path.is_file();
    let sources = builtins();
    let customs = load_customs(&target, &sources)?;
    let source =
        config_io::current_source_scoped(target.root())?.unwrap_or_else(|| "official".to_string());
    let url = source_url(&source, &sources, &customs).unwrap_or_default();
    let registries = config_io::read_private_registries_scoped(target.root());
    let snapshots = config_io::read_snapshots().len();
    let cargo_home = config_io::cargo_home().ok();
    let app_dir = config_io::app_dir().ok();
    let root = target.root().map(|root| root.display().to_string());

    if args.json {
        let value = json!({
            "scope": target.scope(),
            "projectRoot": root,
            "config": path.display().to_string(),
            "configExists": exists,
            "source": source,
            "sourceUrl": if url.is_empty() { serde_json::Value::Null } else { json!(url) },
            "customSources": customs.len(),
            "registries": registries.len(),
            "snapshots": snapshots,
            "cargoHome": cargo_home.map(|path| path.display().to_string()),
            "appDir": app_dir.map(|path| path.display().to_string()),
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(0);
    }

    println!("{:<15}{}", "scope:", target.scope());
    if let Some(root) = root {
        println!("{:<15}{root}", "project:");
    }
    println!("{:<15}{}", "config:", path.display());
    println!("{:<15}{}", "config exists:", yes_no(exists));
    println!("{:<15}{source}", "source:");
    println!(
        "{:<15}{}",
        "source url:",
        if url.is_empty() { "-" } else { &url }
    );
    println!("{:<15}{}", "custom sources:", customs.len());
    println!("{:<15}{}", "registries:", registries.len());
    println!("{:<15}{snapshots}", "snapshots:");
    println!(
        "{:<15}{}",
        "cargo home:",
        display_path(cargo_home.as_deref())
    );
    println!("{:<15}{}", "app dir:", display_path(app_dir.as_deref()));
    Ok(0)
}

fn cmd_list(args: &Args) -> Result<i32> {
    let target = resolve_target(args)?;
    let sources = builtins();
    let customs = load_customs(&target, &sources)?;
    let current =
        config_io::current_source_scoped(target.root())?.unwrap_or_else(|| "official".to_string());

    let mut custom_entries: Vec<(&String, &CustomSource)> = customs.iter().collect();
    custom_entries.sort_by(|left, right| left.0.cmp(right.0));

    if args.json {
        let mut items = Vec::new();
        for source in &sources {
            items.push(json!({
                "id": source.id,
                "kind": "builtin",
                "url": source.url,
                "active": source.id == current,
            }));
        }
        for (id, source) in &custom_entries {
            items.push(json!({
                "id": id,
                "kind": "custom",
                "name": source.name,
                "description": source.description,
                "url": source.url,
                "active": **id == current,
            }));
        }
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(0);
    }

    for source in &sources {
        println!(
            "{} {:<18} {:<8} {}",
            marker(source.id == current),
            source.id,
            "builtin",
            source.url
        );
    }
    for (id, source) in &custom_entries {
        println!(
            "{} {:<18} {:<8} {}",
            marker(**id == current),
            id,
            "custom",
            source.url
        );
    }
    Ok(0)
}

fn cmd_use(args: &Args) -> Result<i32> {
    let id = args
        .words
        .get(1)
        .ok_or_else(|| usage("usage: srmgui use <source-id>"))?
        .trim()
        .to_string();
    let target = resolve_target(args)?;
    let sources = builtins();
    let customs = load_customs(&target, &sources)?;
    let known = sources.iter().any(|source| source.id == id)
        || customs.contains_key(&id)
        || config_sources(&target.config()?)
            .iter()
            .any(|(name, _)| *name == id);
    if !known {
        bail!("unknown source \"{id}\"; run `srmgui list` to see available sources");
    }

    let content = registry::build_config(&id, &sources, &customs);
    let path = match target.root() {
        Some(root) => config_io::write_project_config(root, &content, &id)?,
        None => config_io::write_config(&content, &id)?,
    };
    // 与 GUI 的写入流程一致：同时持久化自定义源元数据。
    let _ = config_io::write_customs(&customs);
    logger::info(
        "cli",
        format!("switched source to \"{id}\" ({})", target.scope()),
    );

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "source": id,
                "scope": target.scope(),
                "config": path.display().to_string(),
            }))?
        );
    } else {
        println!("switched to \"{id}\" ({})", target.scope());
        println!("config: {}", path.display());
    }
    Ok(0)
}

fn cmd_probe(args: &Args) -> Result<i32> {
    let target = resolve_target(args)?;
    let sources = builtins();
    let customs = load_customs(&target, &sources)?;
    let scoped_extra = target
        .config()
        .map(|path| config_sources(&path))
        .unwrap_or_default();

    let ids: Vec<String> = if args.all {
        let mut ids: Vec<String> = sources.iter().map(|source| source.id.clone()).collect();
        ids.extend(customs.keys().cloned());
        ids.sort();
        ids.dedup();
        ids
    } else if args.words.len() > 1 {
        args.words[1..].to_vec()
    } else {
        vec![
            config_io::current_source_scoped(target.root())?
                .unwrap_or_else(|| "official".to_string()),
        ]
    };

    let mut failures = 0usize;
    let mut results = Vec::new();
    for id in &ids {
        let url = source_url(id, &sources, &customs)
            .or_else(|| {
                scoped_extra
                    .iter()
                    .find(|(name, _)| name == id)
                    .map(|(_, url)| url.clone())
            })
            .with_context(|| {
                format!("unknown source \"{id}\"; run `srmgui list` to see available sources")
            })?;
        let report = net::probe(&url);
        let failed = [&report.tcp, &report.http, &report.parse, &report.download]
            .iter()
            .any(|layer| matches!(layer, net::Layer::Failed(_)));
        if failed {
            failures += 1;
        }

        if args.json {
            results.push(json!({
                "id": id,
                "url": url,
                "ok": !failed,
                "tcp": layer_json(&report.tcp),
                "http": layer_json(&report.http),
                "index": layer_json(&report.parse),
                "download": layer_json(&report.download),
            }));
        } else {
            println!("{id}  {url}");
            println!("  {:<10}{}", "tcp:", layer_text(&report.tcp));
            println!("  {:<10}{}", "http:", layer_text(&report.http));
            println!("  {:<10}{}", "index:", layer_text(&report.parse));
            println!("  {:<10}{}", "download:", layer_text(&report.download));
        }
    }

    if args.json {
        println!("{}", serde_json::to_string_pretty(&results)?);
    }
    Ok(if failures > 0 { 1 } else { 0 })
}

fn cmd_custom(args: &Args) -> Result<i32> {
    match args.words.get(1).map(String::as_str) {
        Some("list") => {
            let target = resolve_target(args)?;
            let sources = builtins();
            let customs = load_customs(&target, &sources)?;
            let mut entries: Vec<(&String, &CustomSource)> = customs.iter().collect();
            entries.sort_by(|left, right| left.0.cmp(right.0));

            if args.json {
                let items: Vec<_> = entries
                    .iter()
                    .map(|(id, source)| {
                        json!({
                            "id": id,
                            "name": source.name,
                            "url": source.url,
                            "description": source.description,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&items)?);
            } else if entries.is_empty() {
                println!("no custom sources");
            } else {
                for (id, source) in entries {
                    println!("{:<18} {:<18} {}", id, source.name, source.url);
                }
            }
            Ok(0)
        }
        Some("add") => {
            let id = args
                .words
                .get(2)
                .ok_or_else(|| usage("usage: srmgui custom add <id> --url <url> [--name <name>]"))?
                .trim()
                .to_string();
            if id.is_empty() {
                return Err(usage("source id must not be empty"));
            }
            let url_input = args.url.as_deref().ok_or_else(|| {
                usage(
                    "usage: srmgui custom add <id> --url <url> [--name <name>] [--description <text>]",
                )
            })?;
            let sources = builtins();
            if sources.iter().any(|source| source.id == id) {
                bail!("\"{id}\" is a built-in source");
            }
            let mut customs = config_io::read_customs()?;
            if customs.contains_key(&id) {
                bail!("custom source \"{id}\" already exists");
            }
            let url = util::normalize_registry_url(url_input);
            let name = args
                .name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or(&id)
                .to_string();
            let description = args.description.clone().unwrap_or_default();
            customs.insert(
                id.clone(),
                CustomSource::new(name.clone(), url.clone(), description),
            );
            config_io::write_customs(&customs)?;
            logger::info("cli", format!("added custom source \"{id}\" ({url})"));

            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "added": { "id": id, "name": name, "url": url },
                    }))?
                );
            } else {
                println!("added custom source \"{id}\" ({url})");
                println!("run `srmgui use {id}` to activate it");
            }
            Ok(0)
        }
        Some("remove") => {
            let id = args
                .words
                .get(2)
                .ok_or_else(|| usage("usage: srmgui custom remove <id>"))?
                .trim()
                .to_string();
            let sources = builtins();
            if sources.iter().any(|source| source.id == id) {
                bail!("\"{id}\" is a built-in source");
            }
            let mut customs = config_io::read_customs()?;
            if customs.remove(&id).is_none() {
                bail!("custom source \"{id}\" not found");
            }
            config_io::write_customs(&customs)?;
            logger::info("cli", format!("removed custom source \"{id}\""));

            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({ "removed": { "id": id } }))?
                );
            } else {
                println!("removed custom source \"{id}\"");
                println!("run `srmgui use <source-id>` to rewrite the cargo config without it");
            }
            Ok(0)
        }
        Some("export") => {
            let path = args
                .words
                .get(2)
                .ok_or_else(|| usage("usage: srmgui custom export <path>"))?;
            let builtin = builtin_ids(&builtins());
            let customs: HashMap<String, CustomSource> = config_io::read_customs()?
                .into_iter()
                .filter(|(id, _)| !builtin.contains(id))
                .collect();
            if customs.is_empty() {
                bail!("no custom sources to export");
            }
            let count = customs.len();
            let target = util::with_sources_extension(Path::new(path));
            std::fs::write(&target, config_io::export_customs(&customs))
                .with_context(|| format!("failed to write {}", target.display()))?;
            logger::info(
                "cli",
                format!("exported {count} custom source(s) to {}", target.display()),
            );

            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "path": target.display().to_string(),
                        "count": count,
                    }))?
                );
            } else {
                println!("exported {count} custom source(s) to {}", target.display());
            }
            Ok(0)
        }
        Some("import") => {
            let path = args
                .words
                .get(2)
                .ok_or_else(|| usage("usage: srmgui custom import <path>"))?;
            let text =
                std::fs::read_to_string(path).with_context(|| format!("failed to read {path}"))?;
            let imported = config_io::import_customs(&text)?;
            let builtin = builtin_ids(&builtins());
            let mut customs = config_io::read_customs()?;
            let mut added = 0usize;
            let mut skipped = 0usize;
            for (id, source) in imported {
                if builtin.contains(&id) || customs.contains_key(&id) {
                    skipped += 1;
                    continue;
                }
                customs.insert(id, source);
                added += 1;
            }
            if added > 0 {
                config_io::write_customs(&customs)?;
            }
            logger::info(
                "cli",
                format!("imported {added} custom source(s), skipped {skipped}"),
            );

            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "added": added,
                        "skipped": skipped,
                    }))?
                );
            } else {
                println!("imported {added} source(s), skipped {skipped}");
            }
            Ok(0)
        }
        Some(other) => Err(usage(format!(
            "unknown custom subcommand \"{other}\" (expected list/add/remove/export/import)"
        ))),
        None => Err(usage(
            "usage: srmgui custom <list|add|remove|export|import>",
        )),
    }
}

fn cmd_registry(args: &Args) -> Result<i32> {
    match args.words.get(1).map(String::as_str) {
        Some("list") => {
            let target = resolve_target(args)?;
            let path = target.config()?;
            let registries = config_io::read_private_registries(&path)?;
            if args.json {
                let items: Vec<_> = registries
                    .iter()
                    .map(|entry| {
                        json!({
                            "name": entry.name,
                            "index": entry.index,
                            "hasToken": entry.has_token,
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "scope": target.scope(),
                        "config": path.display().to_string(),
                        "registries": items,
                    }))?
                );
            } else if registries.is_empty() {
                println!("no private registries ({})", path.display());
            } else {
                for entry in registries {
                    println!(
                        "{:<18} {:<5} {}",
                        entry.name,
                        if entry.has_token { "token" } else { "-" },
                        entry.index
                    );
                }
            }
            Ok(0)
        }
        Some("add") => {
            let name = args
                .words
                .get(2)
                .ok_or_else(|| {
                    usage("usage: srmgui registry add <name> --index <url> [--token <token>]")
                })?
                .trim()
                .to_string();
            if !util::valid_registry_name(&name) {
                return Err(usage(format!(
                    "invalid registry name \"{name}\": only letters, digits, \"-\" and \"_\" are allowed"
                )));
            }
            let index_input = args.index.as_deref().ok_or_else(|| {
                usage("usage: srmgui registry add <name> --index <url> [--token <token>]")
            })?;
            let index = util::normalize_registry_index(index_input);
            let target = resolve_target(args)?;
            let path = target.config()?;
            config_io::upsert_private_registry(
                &path,
                target.scope(),
                &name,
                &index,
                args.token.as_deref(),
            )?;
            logger::info(
                "cli",
                format!("saved registry \"{name}\" ({index}) in {}", path.display()),
            );

            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "name": name,
                        "index": index,
                        "hasToken": args.token.is_some(),
                        "scope": target.scope(),
                        "config": path.display().to_string(),
                    }))?
                );
            } else {
                println!(
                    "registry \"{name}\" saved in {} ({})",
                    path.display(),
                    target.scope()
                );
                if args.token.is_some() {
                    println!(
                        "token stored in {}",
                        config_io::credentials_path()?.display()
                    );
                }
            }
            Ok(0)
        }
        Some("remove") => {
            let name = args
                .words
                .get(2)
                .ok_or_else(|| usage("usage: srmgui registry remove <name>"))?
                .trim()
                .to_string();
            let target = resolve_target(args)?;
            let path = target.config()?;
            config_io::remove_private_registry(&path, target.scope(), &name)?;
            logger::info("cli", format!("removed registry \"{name}\""));

            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "removed": { "name": name },
                        "scope": target.scope(),
                    }))?
                );
            } else {
                println!("removed registry \"{name}\" ({})", target.scope());
            }
            Ok(0)
        }
        Some(other) => Err(usage(format!(
            "unknown registry subcommand \"{other}\" (expected list/add/remove)"
        ))),
        None => Err(usage("usage: srmgui registry <list|add|remove>")),
    }
}

fn cmd_snapshot(args: &Args) -> Result<i32> {
    match args.words.get(1).map(String::as_str) {
        Some("list") => {
            let snapshots = config_io::read_snapshots();
            if args.json {
                let items: Vec<_> = snapshots
                    .iter()
                    .enumerate()
                    .map(|(index, snapshot)| {
                        json!({
                            "number": index + 1,
                            "createdAt": snapshot.created_at,
                            "scope": snapshot.scope,
                            "source": snapshot.source,
                            "target": snapshot.target.display().to_string(),
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&items)?);
            } else if snapshots.is_empty() {
                println!("no snapshots");
            } else {
                println!(
                    "{:<4}{:<26}{:<10}{:<16}{}",
                    "#", "created at", "scope", "source", "target"
                );
                for (index, snapshot) in snapshots.iter().enumerate() {
                    println!(
                        "{:<4}{:<26}{:<10}{:<16}{}",
                        index + 1,
                        snapshot.created_at,
                        snapshot.scope,
                        snapshot.source,
                        snapshot.target.display()
                    );
                }
            }
            Ok(0)
        }
        Some("restore") => {
            let selector = args.words.get(2).map(String::as_str).unwrap_or("latest");
            let snapshots = config_io::read_snapshots();
            if snapshots.is_empty() {
                bail!("no snapshots available");
            }
            let snapshot = if selector == "latest" {
                snapshots.first()
            } else {
                let number: usize = selector
                    .parse()
                    .with_context(|| format!("invalid snapshot number \"{selector}\""))?;
                number.checked_sub(1).and_then(|index| snapshots.get(index))
            }
            .with_context(|| format!("snapshot \"{selector}\" not found"))?;

            config_io::restore_snapshot(snapshot)?;
            logger::info(
                "cli",
                format!(
                    "restored snapshot {} into {}",
                    snapshot.created_at,
                    snapshot.target.display()
                ),
            );

            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "restored": {
                            "createdAt": snapshot.created_at,
                            "scope": snapshot.scope,
                            "source": snapshot.source,
                            "target": snapshot.target.display().to_string(),
                        }
                    }))?
                );
            } else {
                println!(
                    "restored snapshot from {} into {}",
                    snapshot.created_at,
                    snapshot.target.display()
                );
            }
            Ok(0)
        }
        Some(other) => Err(usage(format!(
            "unknown snapshot subcommand \"{other}\" (expected list/restore)"
        ))),
        None => Err(usage("usage: srmgui snapshot <list|restore>")),
    }
}

fn cmd_scope(args: &Args) -> Result<i32> {
    let mut settings = config_io::read_settings();
    match args.words.get(1).map(String::as_str) {
        None => {
            let remembered = settings
                .project_root
                .clone()
                .filter(|root| root.join("Cargo.toml").is_file());
            let project = settings.project_scope && remembered.is_some();
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "scope": if project { SCOPE_PROJECT } else { SCOPE_GLOBAL },
                        "projectRoot": remembered.map(|root| root.display().to_string()),
                    }))?
                );
            } else {
                println!(
                    "{:<15}{}",
                    "scope:",
                    if project { SCOPE_PROJECT } else { SCOPE_GLOBAL }
                );
                println!(
                    "{:<15}{}",
                    "project root:",
                    display_path(remembered.as_deref())
                );
            }
            Ok(0)
        }
        Some("global") => {
            settings.project_scope = false;
            config_io::write_settings(&settings)?;
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({ "scope": SCOPE_GLOBAL }))?
                );
            } else {
                println!("default scope is now global");
            }
            Ok(0)
        }
        Some("project") => {
            let root = match &args.project {
                Some(dir) => config_io::detect_project_root(dir).with_context(|| {
                    format!(
                        "no Cargo.toml found in {} or any parent directory",
                        dir.display()
                    )
                })?,
                None => settings
                    .project_root
                    .clone()
                    .filter(|root| root.join("Cargo.toml").is_file())
                    .or_else(|| {
                        std::env::current_dir()
                            .ok()
                            .and_then(|dir| config_io::detect_project_root(&dir))
                    })
                    .context("project scope needs a Cargo project; pass --project <dir>")?,
            };
            settings.project_root = Some(root.clone());
            settings.project_scope = true;
            config_io::write_settings(&settings)?;
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "scope": SCOPE_PROJECT,
                        "projectRoot": root.display().to_string(),
                    }))?
                );
            } else {
                println!("default scope is now project ({})", root.display());
            }
            Ok(0)
        }
        Some(other) => Err(usage(format!(
            "unknown scope \"{other}\" (expected global or project)"
        ))),
    }
}

fn cmd_doctor(args: &Args) -> Result<i32> {
    let target = resolve_target(args)?;
    let path = target.config()?;
    let report = health::scan(&path, target.scope())?;
    let summary = if args.fix && !report.is_healthy() {
        Some(health::fix(&path, target.scope())?)
    } else {
        None
    };

    let code = match &summary {
        Some(summary) if summary.unfixable => 1,
        Some(summary) if summary.fixed > 0 || summary.restored => 0,
        Some(_) => 1,
        None if report.is_healthy() => 0,
        None => 1,
    };

    if args.json {
        let issues: Vec<_> = report
            .issues
            .iter()
            .map(|issue| {
                json!({
                    "kind": issue_kind(issue.kind),
                    "detail": issue.detail,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "config": path.display().to_string(),
                "scope": report.scope,
                "exists": report.exists,
                "healthy": report.is_healthy(),
                "issues": issues,
                "fix": summary.map(|summary| json!({
                    "fixed": summary.fixed,
                    "restored": summary.restored,
                    "unfixable": summary.unfixable,
                })),
            }))?
        );
        return Ok(code);
    }

    println!("{:<15}{}", "config:", path.display());
    println!("{:<15}{}", "scope:", report.scope);
    println!("{:<15}{}", "config exists:", yes_no(report.exists));
    if report.is_healthy() {
        println!("{:<15}{}", "status:", "healthy");
    } else {
        println!(
            "{:<15}{}",
            "status:",
            format!("{} issue(s)", report.issues.len())
        );
        for issue in &report.issues {
            println!("  - {}: {}", issue_kind(issue.kind), issue.detail);
        }
    }
    if let Some(summary) = summary {
        println!("{:<15}{}", "fixed:", summary.fixed);
        println!("{:<15}{}", "restored:", yes_no(summary.restored));
        println!("{:<15}{}", "unfixable:", yes_no(summary.unfixable));
    }
    Ok(code)
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn marker(active: bool) -> &'static str {
    if active { "*" } else { " " }
}

fn display_path(path: Option<&Path>) -> String {
    path.map(|path| path.display().to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn layer_text(layer: &net::Layer) -> String {
    match layer {
        net::Layer::Skipped => "-".to_string(),
        net::Layer::Passed(detail) => detail.clone(),
        net::Layer::Failed(error) => format!("failed: {error}"),
    }
}

fn layer_json(layer: &net::Layer) -> serde_json::Value {
    match layer {
        net::Layer::Skipped => json!({ "status": "skipped" }),
        net::Layer::Passed(detail) => json!({ "status": "passed", "detail": detail }),
        net::Layer::Failed(error) => json!({ "status": "failed", "detail": error }),
    }
}

fn issue_kind(kind: health::IssueKind) -> &'static str {
    match kind {
        health::IssueKind::DuplicateTable => "duplicate-table",
        health::IssueKind::DanglingReplace => "dangling-replace",
        health::IssueKind::OrphanSource => "orphan-source",
        health::IssueKind::Syntax => "syntax",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flags_and_positionals() {
        let args = parse_args(vec![
            "--json".to_string(),
            "use".to_string(),
            "rsproxy".to_string(),
            "--scope=project".to_string(),
        ])
        .unwrap();
        assert!(args.json);
        assert_eq!(args.words, vec!["use".to_string(), "rsproxy".to_string()]);
        assert_eq!(args.scope, Some(true));
    }

    #[test]
    fn parses_value_flags_with_space_or_equals() {
        let args = parse_args(vec![
            "custom".to_string(),
            "add".to_string(),
            "corp".to_string(),
            "--url".to_string(),
            "mirror.example.com".to_string(),
            "--name=Corp".to_string(),
        ])
        .unwrap();
        assert_eq!(args.url.as_deref(), Some("mirror.example.com"));
        assert_eq!(args.name.as_deref(), Some("Corp"));
        assert_eq!(args.words.len(), 3);
    }

    #[test]
    fn rejects_unknown_or_incomplete_options() {
        assert!(parse_args(vec!["--nope".to_string()]).is_err());
        assert!(parse_args(vec!["status".to_string(), "--scope".to_string()]).is_err());
        assert!(parse_args(vec!["status".to_string(), "--scope=moon".to_string()]).is_err());
        assert!(parse_args(vec!["-x".to_string()]).is_err());
    }

    #[test]
    fn reads_registry_urls_from_config() {
        let dir = std::env::temp_dir().join(format!("srmgui-cli-sources-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "[source.crates-io]\nreplace-with = \"corp\"\n\n\
             [source.corp]\nregistry = \"sparse+https://corp.example/index/\"\n",
        )
        .unwrap();

        assert_eq!(
            config_sources(&path),
            vec![(
                "corp".to_string(),
                "sparse+https://corp.example/index/".to_string()
            )]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn layer_output_covers_all_states() {
        assert_eq!(layer_text(&net::Layer::Skipped), "-");
        assert_eq!(
            layer_text(&net::Layer::Passed("12 ms".to_string())),
            "12 ms"
        );
        assert_eq!(
            layer_text(&net::Layer::Failed("timeout".to_string())),
            "failed: timeout"
        );
        assert_eq!(layer_json(&net::Layer::Skipped)["status"], "skipped");
        assert_eq!(
            layer_json(&net::Layer::Failed("x".to_string()))["status"],
            "failed"
        );
    }
}
