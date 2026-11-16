rust_i18n::i18n!("locales", fallback = "zh-CN");

mod add_form;
mod app;
mod cache;
mod cache_dialog;
mod cli;
mod config_dialog;
mod config_io;
mod diff;
mod health;
mod log_panel;
mod logger;
mod manifest;
mod model;
mod net;
mod private_list;
mod registry;
mod root_view;
mod settings_dialog;
mod source_list;
mod ui;
mod update;
mod util;

use app::CargoMirrorApp;
use gpui_kit::AppContext as _;
use gpui_kit::WindowOptions;
use gpui_kit::actions;
use gpui_kit::component::TitleBar;
use root_view::RootView;

actions!(cargo_mirror_switcher, [Quit, ToggleTheme]);

fn main() {
    // 同一个可执行文件同时提供 GUI 与命令行子命令：先按参数执行 CLI，
    // 无参数（或显式 `gui` 子命令）时再启动界面。
    if let Some(code) = cli::run() {
        std::process::exit(code);
    }

    #[cfg(windows)]
    detach_console();

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            let app = CargoMirrorApp::new(cx);
            let _ = gpui_kit::open_window(
                WindowOptions {
                    titlebar: Some(gpui_kit::TitlebarOptions {
                        title: Some(rust_i18n::t!("app.title").to_string().into()),
                        ..TitleBar::title_bar_options()
                    }),
                    app_owns_titlebar_drag: true,
                    window_bounds: Some(gpui_kit::WindowBounds::Windowed(gpui_kit::Bounds {
                        origin: gpui_kit::point(gpui_kit::px(200.), gpui_kit::px(200.)),
                        size: gpui_kit::size(gpui_kit::px(760.), gpui_kit::px(560.)),
                    })),
                    ..Default::default()
                },
                cx,
                move |window, cx| cx.new(|cx| RootView::new(window, cx, app)),
            );
        });
}

/// Windows 下二进制为控制台子系统（保证 `srmgui status` 等子命令在
/// cmd / PowerShell / CI 中能被正确等待并拿到输出与退出码），
/// 因此 GUI 模式启动时需要主动脱离控制台。
#[cfg(windows)]
fn detach_console() {
    unsafe extern "system" {
        fn GetConsoleProcessList(list: *mut u32, count: u32) -> u32;
        fn GetConsoleWindow() -> *mut std::ffi::c_void;
        fn ShowWindow(window: *mut std::ffi::c_void, command: i32) -> i32;
        fn FreeConsole() -> i32;
    }

    // 双击启动时系统会为本进程单独分配控制台窗口，先隐藏再脱离；
    // 从终端启动时控制台与父进程共享，只脱离，不影响用户正在使用的终端。
    const SW_HIDE: i32 = 0;
    unsafe {
        let mut processes = [0u32; 2];
        let count = GetConsoleProcessList(processes.as_mut_ptr(), processes.len() as u32);
        if count == 1 {
            let window = GetConsoleWindow();
            if !window.is_null() {
                ShowWindow(window, SW_HIDE);
            }
        }
        FreeConsole();
    }
}
