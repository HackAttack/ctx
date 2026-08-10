fn main() {
    println!("cargo:rustc-check-cfg=cfg(ctx_release_qualification)");
    println!("cargo:rustc-check-cfg=cfg(ctx_upgrade_engine_test_support)");
    println!("cargo:rustc-check-cfg=cfg(ctx_cli_bazel_test)");
}
