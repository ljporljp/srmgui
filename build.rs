use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/logo.ico");
    println!("cargo:rerun-if-changed=locales");

    // 把应用图标写入 Windows 可执行文件资源，窗口与任务栏会使用该图标。
    #[cfg(windows)]
    let _ = embed_resource::compile("assets/app.rc", embed_resource::NONE);

    // rust-i18n 的 i18n! 宏在编译期直接读取 locales 下的 YAML，cargo 不会把这些文件
    // 当作依赖，只改 YAML 时不会重新编译。这里把语言文件内容哈希写入编译环境变量，
    // 内容变化时构建脚本输出随之变化，确保语言修改一定会重新编译进二进制。
    let mut paths: Vec<_> = std::fs::read_dir(Path::new("locales"))
        .map(|entries| entries.filter_map(Result::ok).map(|e| e.path()).collect())
        .unwrap_or_default();
    paths.sort();

    let mut hasher = DefaultHasher::new();
    for path in paths {
        if path.extension().and_then(|ext| ext.to_str()) != Some("yml") {
            continue;
        }
        if let Ok(content) = std::fs::read(&path) {
            path.hash(&mut hasher);
            content.hash(&mut hasher);
        }
    }

    println!("cargo:rustc-env=SRMGUI_LOCALES_HASH={:x}", hasher.finish());
}
