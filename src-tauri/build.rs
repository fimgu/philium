fn main() {
    println!("cargo:rustc-env=PHILIUM_TARGET_TRIPLE={}", std::env::var("TARGET").expect("Cargo target triple"));
    tauri_build::build()
}
