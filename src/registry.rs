use std::collections::HashMap;

use crate::config_io::CustomSource;

/// One Cargo registry source.
#[derive(Debug, Clone)]
pub struct Registry {
    pub id: &'static str,
    pub name: &'static str,
    pub url: &'static str,
    pub description: &'static str,
}

/// All built-in registries. Order = display order in the list.
pub fn builtins() -> Vec<Registry> {
    vec![
        Registry {
            id: "rsproxy",
            name: "RsProxy（字节）",
            url: "sparse+https://rsproxy.cn/index/",
            description: "国内实测最稳，推荐日常使用",
        },
        Registry {
            id: "aliyun",
            name: "阿里云",
            url: "sparse+https://mirrors.aliyun.com/crates.io-index/",
            description: "备用源，偶发索引通但下载 404",
        },
        Registry {
            id: "ustc",
            name: "中科大 USTC",
            url: "sparse+https://mirrors.ustc.edu.cn/crates.io-index/",
            description: "文档仍在，连通性波动较大",
        },
        Registry {
            id: "tuna",
            name: "清华 TUNA",
            url: "sparse+https://mirrors.tuna.tsinghua.edu.cn/crates.io-index/",
            description: "帮助文档仍在，偶发无响应",
        },
        Registry {
            id: "sjtu",
            name: "上海交大 SJTUG",
            url: "sparse+https://mirrors.sjtug.sjtu.edu.cn/crates.io-index/",
            description: "高校源，适合应急",
        },
        Registry {
            id: "official",
            name: "官方 crates.io",
            url: "sparse+https://index.crates.io/",
            description: "默认源，cargo publish 前建议切回",
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
