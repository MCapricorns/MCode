//! MCode desktop application entry point.

use mcode_config::{HomeEnv, HomeLayout};
use mcode_desktop::workspace;

fn main() {
    let home = match HomeLayout::from_env(HomeEnv::from_process()) {
        Ok(home) => home,
        Err(error) => {
            eprintln!("mcode home unavailable: {error}");
            std::process::exit(1);
        }
    };
    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        workspace::open_window(home, cx);
    });
}
