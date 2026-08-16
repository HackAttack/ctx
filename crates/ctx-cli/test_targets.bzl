"""Compatibility shim for CLI-owned binary-contract declarations."""

load(
    "//crates/ctx-cli-contract-tests:test_targets.bzl",
    _CTX_CLI_RUSTC_FLAGS = "CTX_CLI_RUSTC_FLAGS",
    _ctx_cli_contract_test = "ctx_cli_contract_test",
    _ctx_cli_test_data = "ctx_cli_test_data",
)

CTX_CLI_RUSTC_FLAGS = _CTX_CLI_RUSTC_FLAGS

def ctx_cli_test_data():
    return _ctx_cli_test_data()

def ctx_cli_integration_test(
        name,
        src,
        binary = ":ctx",
        crate_features = [],
        extra_crates = [],
        extra_env = {},
        extra_compile_data = [],
        extra_data = [],
        extra_deps = [],
        extra_srcs = [],
        tags = [],
        test_support = ["base"]):
    _ctx_cli_contract_test(
        name = name,
        src = src,
        binary = binary,
        crate_features = crate_features,
        extra_crates = extra_crates,
        extra_env = extra_env,
        extra_compile_data = extra_compile_data,
        extra_data = extra_data,
        extra_deps = extra_deps,
        extra_srcs = extra_srcs,
        support_shim = "tests/support/mod.rs",
        tags = tags,
        test_support = test_support,
    )
