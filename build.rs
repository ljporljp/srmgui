fn main() {
    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/logo.ico");

    // 把应用图标写入 Windows 可执行文件资源，窗口与任务栏会使用该图标。
    #[cfg(windows)]
    let _ = embed_resource::compile("assets/app.rc", embed_resource::NONE);
}
