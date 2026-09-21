//! MYCode desktop application entry point.
//
// Release builds run windowed: no console flashes on Windows. Debug builds
// keep the console so eprintln diagnostics stay visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use mycode_config::{HomeEnv, HomeLayout};
use mycode_desktop::workspace;

fn main() {
    // Remove staging directories left behind by earlier self-updates.
    mycode_app::cleanup_stale_stages();
    let home = match HomeLayout::from_env(HomeEnv::from_process()) {
        Ok(home) => home,
        Err(error) => {
            eprintln!("mycode home unavailable: {error}");
            std::process::exit(1);
        }
    };
    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            workspace::open_window(home, cx);
        });
}
