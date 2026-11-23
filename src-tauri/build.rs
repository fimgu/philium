fn main() {
    println!("cargo:rustc-env=PHILIUM_TARGET_TRIPLE={}", std::env::var("TARGET").expect("Cargo target triple"));
    link_clang_rt();
    export_app_id();
    tauri_build::build()
}

/// ONNX Runtime's CoreML objects call `__isPlatformVersionAtLeast` (emitted for `@available`), which
/// lives in clang's `libclang_rt.osx.a`. Rust links with `-nodefaultlibs`, so release (LTO) builds
/// fail to resolve it unless the runtime is linked explicitly.
fn link_clang_rt() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let Ok(out) = std::process::Command::new("xcrun").args(["clang", "-print-resource-dir"]).output() else { return };
    let dir = std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()).join("lib/darwin");
    if dir.join("libclang_rt.osx.a").exists() {
        println!("cargo:rustc-link-search=native={}", dir.display());
        println!("cargo:rustc-link-lib=static=clang_rt.osx");
    }
}

/// `src/app_id.rs` reads the bundle identifier from here, so it is defined once.
fn export_app_id() {
    println!("cargo:rerun-if-changed=tauri.conf.json");
    let conf = std::fs::read_to_string("tauri.conf.json").expect("tauri.conf.json");
    let id = conf
        .split("\"identifier\"")
        .nth(1)
        .and_then(|r| r.split('"').nth(1))
        .expect("identifier in tauri.conf.json");
    println!("cargo:rustc-env=PHILIUM_APP_ID={id}");
}
