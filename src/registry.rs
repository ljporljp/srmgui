use std::collections::HashMap;

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
pub fn build_config(selected: &str, customs: &HashMap<String, String>) -> String {
    let mut out = String::new();
    out.push_str("[source.crates-io]\n");
    out.push_str(&format!("replace-with = \"{selected}\"\n\n"));

    for r in builtins() {
        out.push_str(&format!("[source.{}]\n", r.id));
        out.push_str(&format!("registry = \"{}\"\n\n", r.url));
    }

    for (id, url) in customs {
        let url = if url.starts_with("sparse+") || url.starts_with("http") {
            url.clone()
        } else {
            format!("sparse+https://{url}/")
        };
        out.push_str(&format!("[source.{id}]\n"));
        out.push_str(&format!("registry = \"{url}\"\n\n"));
    }

    out.push_str("[net]\n");
    out.push_str("git-fetch-with-cli = true\n");
    out
}
