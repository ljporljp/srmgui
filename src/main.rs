#![cfg_attr(windows, windows_subsystem = "windows")]

mod add_form;
mod app;
mod config_io;
mod logger;
mod log_panel;
mod model;
mod net;
mod registry;
mod root_view;
mod source_list;
mod ui;
mod util;

use app::CargoMirrorApp;
use gpui_kit::AppContext as _;
use gpui_kit::WindowOptions;
use gpui_kit::actions;
use gpui_kit::component::TitleBar;
use root_view::RootView;

actions!(cargo_mirror_switcher, [Quit, ToggleTheme]);

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            let app = CargoMirrorApp::new(cx);
            let _ = gpui_kit::open_window(
                WindowOptions {
                    titlebar: Some(gpui_kit::TitlebarOptions {
                        title: Some("Cargo 镜像源切换工具".into()),
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
