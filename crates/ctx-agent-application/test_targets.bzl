"""Bazel-only final-binary contracts owned by ctx-agent-application."""

load("@crates//:defs.bzl", "crate_deps")
load("//tools/bazel:binary_contracts.bzl", "ctx_binary_contract_test")

_CONTRACT_SUPPORT_SRCS = [
    "tests/contracts/support.rs",
    "tests/contracts/support/mcp.rs",
]

_CONTRACT_SUPPORT_CRATES = [
    "assert_cmd",
    "predicates",
    "serde_json",
    "tempfile",
    "uuid",
]

_CONTRACT_FIXTURE_CRATES = [
    "rusqlite",
    "sha2",
]

def agent_application_binary_contract(
        name,
        src,
        extra_crates = [],
        extra_deps = [],
        extra_srcs = [],
        fixtures = False,
        tags = []):
    ctx_binary_contract_test(
        name = name,
        src = src,
        binary = "//crates/ctx-cli:ctx",
        cargo_manifest_dir = "crates/ctx-agent-application",
        support_deps = crate_deps(_CONTRACT_SUPPORT_CRATES + (_CONTRACT_FIXTURE_CRATES if fixtures else [])) + (["//crates/ctx-history-index:lib"] if fixtures else []),
        support_srcs = _CONTRACT_SUPPORT_SRCS,
        support_rustc_flags = ["--cfg=ctx_agent_application_contract_fixtures"] if fixtures else [],
        extra_crates = extra_crates,
        extra_compile_data = [
            "//:ctx_bundled_skills",
            "//:ctx_embedded_docs",
        ],
        extra_data = ["//:public_test_fixtures"],
        extra_deps = extra_deps,
        extra_srcs = extra_srcs,
        tags = tags,
    )
