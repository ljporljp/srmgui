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
        out.push_str(&format!("replace-with = {}\n", toml_string(selected)));
    }
    out.push('\n');

    let builtin_ids: std::collections::HashSet<&str> =
        sources.iter().map(|r| r.id.as_str()).collect();

    for r in sources {
        // `official` is crates.io itself; never emit a [source.official] block.
        if r.id == "official" {
            continue;
        }
        out.push_str(&format!("[source.{}]\n", toml_key(&r.id)));
        out.push_str(&format!("registry = \"{}\"\n\n", r.url));
    }

    // 自定义源按标识排序输出，保证同样的输入总是生成同样的配置（避免文件抖动）。
    let mut custom_ids: Vec<&String> = customs.keys().collect();
    custom_ids.sort();
    for id in custom_ids {
        // Skip customs that shadow a built-in id to avoid duplicate sections.
        if builtin_ids.contains(id.as_str()) {
            continue;
        }
        let url = crate::util::normalize_registry_url(&customs[id].url);
        out.push_str(&format!("[source.{}]\n", toml_key(id)));
        out.push_str(&format!("registry = \"{url}\"\n\n"));
    }

    out.push_str("[net]\n");
    out.push_str("git-fetch-with-cli = true\n");
    out
}

/// 生成 TOML 字符串字面量：值必须始终带引号（如 `replace-with = "aliyun"`），
/// 并对反斜杠、双引号与控制字符转义。
fn toml_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// 生成安全的 TOML 键：普通标识直接输出，含特殊字符时加引号转义，
/// 避免用户填写的标识破坏整份配置文件。
fn toml_key(key: &str) -> String {
    let plain = !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if plain {
        key.to_string()
    } else {
        format!("\"{}\"", key.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::ManifestSource;

    fn builtin(id: &str) -> ManifestSource {
        ManifestSource::new(id, id, "", format!("sparse+https://{id}.example.cn/"))
    }

    #[test]
    fn custom_sources_are_emitted_in_stable_order() {
        let mut customs = HashMap::new();
        customs.insert(
            "zeta".to_string(),
            CustomSource::new("zeta", "zeta.example.cn", ""),
        );
        customs.insert(
            "alpha".to_string(),
            CustomSource::new("alpha", "alpha.example.cn", ""),
        );

        let text = build_config("alpha", &[builtin("official")], &customs);
        let alpha = text.find("[source.alpha]").expect("alpha block");
        let zeta = text.find("[source.zeta]").expect("zeta block");
        assert!(alpha < zeta, "自定义源必须按标识稳定排序输出");
        assert!(text.contains("registry = \"sparse+https://alpha.example.cn/\""));
    }

    #[test]
    fn replace_with_is_always_a_quoted_string() {
        let text = build_config("aliyun", &[builtin("aliyun")], &HashMap::new());
        assert!(text.contains("replace-with = \"aliyun\""));
        let parsed = text
            .parse::<toml::Table>()
            .expect("生成的配置必须是合法 TOML");
        assert_eq!(
            parsed["source"]["crates-io"]["replace-with"].as_str(),
            Some("aliyun")
        );
    }

    #[test]
    fn unsafe_custom_ids_are_quoted_to_keep_config_valid() {
        let mut customs = HashMap::new();
        customs.insert(
            "evil\"]".to_string(),
            CustomSource::new("evil", "evil.example.cn", ""),
        );

        let text = build_config("evil\"]", &[builtin("official")], &customs);
        let parsed = text
            .parse::<toml::Table>()
            .expect("生成的配置必须是合法 TOML");
        assert_eq!(
            parsed["source"]["evil\"]"]["registry"].as_str(),
            Some("sparse+https://evil.example.cn/")
        );
    }
}
