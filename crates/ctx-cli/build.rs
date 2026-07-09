use std::env;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(ctx_semantic_fastembed)");
    println!("cargo:rustc-check-cfg=cfg(ctx_sqlite_vec)");

    let os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    let supported = match os.as_str() {
        "linux" => matches!(arch.as_str(), "x86_64" | "aarch64") && target_env == "gnu",
        "macos" => arch == "aarch64",
        "windows" => arch == "x86_64" && target_env == "msvc",
        _ => false,
    };

    if supported {
        println!("cargo:rustc-cfg=ctx_semantic_fastembed");
    }

    if os == "linux" && matches!(arch.as_str(), "x86_64" | "aarch64") && target_env == "gnu" {
        println!("cargo:rustc-cfg=ctx_sqlite_vec");
    }
}
