use std::collections::HashMap;

use crate::config_io::CustomSource;
use crate::manifest::ManifestSource;

/// One Cargo registry source. 展示名称与说明是本地化文案，按 `registry.{id}.*` 查找。
#[derive(Debug, Clone)]
pub struct Registry {
    pub id: &'static str,
    pub url: &'static str,
    /// 所属地区代码：`CN`（中国大陆）/ `GLOBAL`（全球），用于按 IP 地区推荐。
    pub region: &'static str,
    /// 所属省份代码（ISO 3166-2:CN，如 `ZJ`）；跨省调度或全球源为空。
    pub province: &'static str,
}

/// 编译期内置的兜底源清单。远端订阅与本地缓存都不可用时使用，顺序即展示顺序。
pub fn builtins() -> Vec<Registry> {
    vec![
        Registry {
            id: "rsproxy",
            url: "sparse+https://rsproxy.cn/index/",
            region: "CN",
            province: "BJ",
        },
        Registry {
            id: "aliyun",
            url: "sparse+https://mirrors.aliyun.com/crates.io-index/",
            region: "CN",
            province: "ZJ",
        },
        Registry {
            id: "ustc",
            url: "sparse+https://mirrors.ustc.edu.cn/crates.io-index/",
            region: "CN",
            province: "AH",
        },
        Registry {
            id: "tuna",
            url: "sparse+https://mirrors.tuna.tsinghua.edu.cn/crates.io-index/",
            region: "CN",
            province: "BJ",
        },
        Registry {
            id: "sjtu",
            url: "sparse+https://mirrors.sjtug.sjtu.edu.cn/crates.io-index/",
            region: "CN",
            province: "SH",
        },
        Registry {
            id: "huawei",
            url: "sparse+https://mirrors.huaweicloud.com/crates.io-index/",
            region: "CN",
            province: "GD",
        },
        Registry {
            id: "cernet",
            url: "sparse+https://mirrors.cernet.edu.cn/crates.io-index/",
            region: "CN",
            province: "",
        },
        Registry {
            id: "nju",
            url: "sparse+https://mirrors.nju.edu.cn/crates.io-index/",
            region: "CN",
            province: "JS",
        },
        Registry {
            id: "bfsu",
            url: "sparse+https://mirrors.bfsu.edu.cn/crates.io-index/",
            region: "CN",
            province: "BJ",
        },
        Registry {
            id: "sdu",
            url: "sparse+https://mirrors.sdu.edu.cn/crates.io-index/",
            region: "CN",
            province: "SD",
        },
        Registry {
            id: "hust",
            url: "sparse+https://mirrors.hust.edu.cn/crates.io-index/",
            region: "CN",
            province: "HB",
        },
        Registry {
            id: "zju",
            url: "sparse+https://mirrors.zju.edu.cn/crates.io-index/",
            region: "CN",
            province: "ZJ",
        },
        Registry {
            id: "neusoft",
            url: "sparse+https://mirrors.neusoft.edu.cn/crates.io-index/",
            region: "CN",
            province: "LN",
        },
        Registry {
            id: "nyist",
            url: "sparse+https://mirror.nyist.edu.cn/crates.io-index/",
            region: "CN",
            province: "HA",
        },
        Registry {
            id: "official",
            url: "sparse+https://index.crates.io/",
            region: "GLOBAL",
            province: "",
        },
    ]
}

/// Build the `[source.crates-io]` replacement block for a chosen source id.
///
/// `sources` 是当前生效的内置源清单（可能来自远端订阅），`customs` 是用户自定义源。
///
/// Duplicate filtering rules applied while writing:
/// - The built-in `official` source points at crates.io itself and must NOT be
///   emitted as its own `[source.official]` block — cargo forbids defining a
///   source that resolves to the same registry as `crates-io`. Selecting it
///   simply means "use the default crates.io", so `replace-with` is omitted.
/// - Custom sources whose id collides with a built-in source are skipped,
///   since the built-in block is always emitted first. A duplicate
///   `[source.<id>]` section would produce invalid / redundant TOML.
pub fn build_config(
    selected: &str,
    sources: &[ManifestSource],
    customs: &HashMap<String, CustomSource>,
) -> String {
    let mut out = String::new();
    out.push_str("[source.crates-io]\n");
    if selected != "official" {
        out.push_str(&format!("replace-with = \"{selected}\"\n"));
    }
    out.push('\n');

    let builtin_ids: Vec<&str> = sources.iter().map(|r| r.id.as_str()).collect();

    for r in sources {
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
