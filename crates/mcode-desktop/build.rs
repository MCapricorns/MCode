//! Embeds the application icon as Windows resource id 1, which gpui's
//! window class loads for the title bar, taskbar, and alt-tab entries.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");
    if std::path::Path::new("assets/icon.ico").exists() {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/icon.ico");
        resource
            .compile()
            .expect("embed the application icon resource");
    }
}
