fn main() {
    println!("cargo:rustc-check-cfg=cfg(ctx_semantic_fastembed)");
    println!("cargo:rustc-check-cfg=cfg(ctx_sqlite_vec)");

    // Current binaries are intentionally lexical-only. The previous x64-only
    // native semantic dependencies imposed an undocumented CPU floor and made
    // otherwise identical platform artifacts behave differently.
}
