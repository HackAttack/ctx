"""Reusable Bazel-only contracts for executing the final ctx binary."""

load("@crates//:defs.bzl", "crate_deps")
load(
    "//tools/bazel:binary_contracts.bzl",
    "CTX_BINARY_CONTRACT_RUSTC_FLAGS",
    "ctx_binary_contract_test",
)

CTX_CLI_RUSTC_FLAGS = CTX_BINARY_CONTRACT_RUSTC_FLAGS

_CTX_CLI_TEST_SUPPORT = {
    "base": struct(
        crates = [
            "assert_cmd",
            "predicates",
            "serde_json",
            "tempfile",
            "uuid",
        ],
        deps = [],
        rustc_flags = [],
        srcs = ["//crates/ctx-cli-contract-tests:contract_support_base"],
    ),
    "fixtures": struct(
        crates = [
            "libc",
            "rusqlite",
            "sha2",
            "windows-sys",
        ],
        deps = ["//crates/ctx-history-index:lib"],
        rustc_flags = ["--cfg=ctx_cli_test_support_fixtures"],
        srcs = ["//crates/ctx-cli-contract-tests:contract_support_fixtures"],
    ),
    "upgrade": struct(
        crates = [
            "base64",
            "chrono",
            "flate2",
            "ring",
            "sha2",
            "tar",
        ],
        deps = ["//crates/ctx-history-core:lib"],
        rustc_flags = ["--cfg=ctx_cli_test_support_upgrade"],
        srcs = ["//crates/ctx-cli-contract-tests:contract_support_upgrade"],
    ),
}

def _ctx_cli_test_support(groups):
    crates = []
    deps = []
    rustc_flags = []
    srcs = []
    seen = {}
    seen_crates = {}
    seen_deps = {}
    seen_rustc_flags = {}
    seen_srcs = {}
    for group in groups:
        if group not in _CTX_CLI_TEST_SUPPORT:
            fail("unknown ctx CLI test support group %r; expected one of %s" % (
                group,
                sorted(_CTX_CLI_TEST_SUPPORT.keys()),
            ))
        if group in seen:
            fail("duplicate ctx CLI test support group %r" % group)
        seen[group] = True
        support = _CTX_CLI_TEST_SUPPORT[group]
        for crate in support.crates:
            if crate not in seen_crates:
                seen_crates[crate] = True
                crates.append(crate)
        for dep in support.deps:
            if dep not in seen_deps:
                seen_deps[dep] = True
                deps.append(dep)
        for flag in support.rustc_flags:
            if flag not in seen_rustc_flags:
                seen_rustc_flags[flag] = True
                rustc_flags.append(flag)
        for src in support.srcs:
            if src not in seen_srcs:
                seen_srcs[src] = True
                srcs.append(src)
    return struct(
        deps = crate_deps(crates) + deps,
        rustc_flags = rustc_flags,
        srcs = srcs,
    )

def ctx_cli_test_data():
    return [
        "//:public_test_fixtures",
        "//crates/ctx-cli:integration_test_data",
    ]

def ctx_cli_contract_test(
        name,
        src,
        binary = "//crates/ctx-cli:ctx",
        cargo_manifest_dir = "crates/ctx-cli",
        cargo_toml_env_vars = "//crates/ctx-cli:cargo_toml_env_vars",
        crate_features = [],
        extra_crates = [],
        extra_env = {},
        extra_compile_data = [],
        extra_data = [],
        deps = [],
        extra_deps = [],
        extra_srcs = [],
        support_shim = None,
        tags = [],
        test_support = ["base"]):
    test_support = _ctx_cli_test_support(test_support)
    support_deps = [
        dep
        for dep in test_support.deps
        if dep not in deps and dep not in extra_deps
    ]
    support_srcs = test_support.srcs
    if support_shim != None:
        support_srcs = [support_shim] + support_srcs
    ctx_binary_contract_test(
        name = name,
        src = src,
        binary = binary,
        cargo_manifest_dir = cargo_manifest_dir,
        support_deps = support_deps,
        support_srcs = support_srcs,
        support_rustc_flags = test_support.rustc_flags,
        crate_features = crate_features,
        extra_crates = extra_crates,
        extra_env = extra_env,
        extra_compile_data = [
            "//:ctx_bundled_skills",
            "//:ctx_embedded_docs",
            "//crates/ctx-cli:integration_test_compile_data",
        ] + extra_compile_data,
        extra_data = ctx_cli_test_data() + extra_data,
        extra_deps = deps + extra_deps,
        extra_srcs = extra_srcs,
        rustc_env_files = [cargo_toml_env_vars],
        tags = tags,
    )
