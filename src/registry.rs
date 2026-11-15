use std::collections::HashMap;

use crate::config_io::CustomSource;

/// One Cargo registry source. 展示名称与说明是本地化文案，按 `registry.{id}.*` 查找。
#[derive(Debug, Clone)]
pub struct Registry {
    pub id: &'static str,
    pub url: &'static str,
}

/// All built-in registries. Order = display order in the list.
pub fn builtins() -> Vec<Registry> {
    vec![
        Registry {
            id: "rsproxy",
            url: "sparse+https://rsproxy.cn/index/",
        },
        Registry {
            id: "aliyun",
            url: "sparse+https://mirrors.aliyun.com/crates.io-index/",
        },
        Registry {
            id: "ustc",
            url: "sparse+https://mirrors.ustc.edu.cn/crates.io-index/",
        },
        Registry {
            id: "tuna",
            url: "sparse+https://mirrors.tuna.tsinghua.edu.cn/crates.io-index/",
        },
        Registry {
            id: "sjtu",
            url: "sparse+https://mirrors.sjtug.sjtu.edu.cn/crates.io-index/",
        },
        Registry {
            id: "official",
            url: "sparse+https://index.crates.io/",
        },
    ]
}

/// Build the `[source.crates-io]` replacement block for a chosen source id.
///
/// Duplicate filtering rules applied while writing:
/// - The built-in `official` source points at crates.io itself and must NOT be
///   emitted as its own `[source.official]` block — cargo forbids defining a
///   source that resolves to the same registry as `crates-io`. Selecting it
///   simply means "use the default crates.io", so `replace-with` is omitted.
/// - Custom sources whose id collides with a built-in source are skipped,
///   since the built-in block is always emitted first. A duplicate
///   `[source.<id>]` section would produce invalid / redundant TOML.
pub fn build_config(selected: &str, customs: &HashMap<String, CustomSource>) -> String {
    let mut out = String::new();
    out.push_str("[source.crates-io]\n");
    if selected != "official" {
        out.push_str(&format!("replace-with = \"{selected}\"\n"));
    }
    out.push('\n');

    let builtin_ids: Vec<&str> = builtins().iter().map(|r| r.id).collect();

    for r in builtins() {
        // `official` is crates.io itself; never emit a [source.official] block.
        if r.id == "official" {
            continue;
        }
        out.push_str(&format!("[source.{}]\n", r.id));
        out.push_str(&format!("registry = \"{}\"\n\n", r.url));
    }

    for (id, src) in customs {
        // Skip customs that shadow a built-in id to avoid duplicate sections.
        if builtin_ids.contains(&id.as_str()) {
            continue;
        }
        let url = if src.url.starts_with("sparse+") || src.url.starts_with("http") {
            src.url.clone()
        } else {
            format!("sparse+https://{}/", src.url)
        };
        out.push_str(&format!("[source.{id}]\n"));
        out.push_str(&format!("registry = \"{url}\"\n\n"));
    }

    out.push_str("[net]\n");
    out.push_str("git-fetch-with-cli = true\n");
    out
}
