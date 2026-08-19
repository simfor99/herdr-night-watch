fn main() {
    println!("cargo:rerun-if-changed=assets/herdr-nachtwaechter.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").ok().as_deref() != Some("windows") {
        return;
    }
    let mut resource = winresource::WindowsResource::new();
    resource.set_icon("assets/herdr-nachtwaechter.ico");
    resource.set("ProductName", "Herdr-Nachtwächter");
    resource.set("FileDescription", "Herdr-Nachtwächter");
    resource.set("LegalCopyright", "Copyright (c) Simon Formanowski");
    if std::env::var("CARGO_CFG_TARGET_ENV").ok().as_deref() == Some("gnu") {
        resource.set_windres_path("x86_64-w64-mingw32-windres");
        resource.set_ar_path("x86_64-w64-mingw32-ar");
    }
    resource.compile().expect("embed Windows EXE icon");
}
