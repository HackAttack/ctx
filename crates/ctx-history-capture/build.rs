fn main() {
    println!("cargo:rustc-check-cfg=cfg(ctx_codex_causal_qualification)");
    println!("cargo:rerun-if-env-changed=CTX_CODEX_CAUSAL_QUALIFICATION_BUILD");
    if std::env::var("CTX_CODEX_CAUSAL_QUALIFICATION_BUILD").as_deref() == Ok("1") {
        println!("cargo:rustc-cfg=ctx_codex_causal_qualification");
    }
}
