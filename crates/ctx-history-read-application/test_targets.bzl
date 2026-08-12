"""Bazel-only final-binary contracts owned by ctx-history-read-application."""

load("//tools/bazel:binary_contracts.bzl", "ctx_binary_contract_test")

_CONTRACT_SUPPORT_SRCS = [
    "tests/support/mod.rs",
    "tests/support/assertions.rs",
    "tests/support/daemon.rs",
    "tests/support/fixtures.rs",
    "tests/support/mcp.rs",
    "tests/support/native_fixtures.rs",
    "tests/support/runner.rs",
]

_CONTRACT_SUPPORT_DEPS = [
    "@crates//:assert_cmd",
    "@crates//:libc",
    "@crates//:predicates",
    "@crates//:rusqlite",
    "@crates//:serde_json",
    "@crates//:tempfile",
    "@crates//:windows-sys",
]

def history_read_binary_contract(
        name,
        src,
        extra_deps = [],
        extra_env = {},
        extra_srcs = [],
        tags = []):
    ctx_binary_contract_test(
        name = name,
        src = src,
        binary = "//crates/ctx-cli:ctx",
        cargo_manifest_dir = "crates/ctx-history-read-application",
        support_deps = _CONTRACT_SUPPORT_DEPS + [
            "//crates/ctx-history-index:lib",
        ],
        support_srcs = _CONTRACT_SUPPORT_SRCS,
        support_rustc_flags = ["--cfg=ctx_cli_test_support_fixtures"],
        extra_compile_data = [
            "//:ctx_bundled_skills",
            "//:ctx_embedded_docs",
        ],
        extra_data = ["//:public_test_fixtures"],
        extra_deps = extra_deps,
        extra_env = extra_env,
        extra_srcs = extra_srcs,
        tags = tags,
    )
