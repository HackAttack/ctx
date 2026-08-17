"""Macros for explicit non-Rust repository gates."""

load("@rules_shell//shell:sh_test.bzl", "sh_test")

def non_rust_gate(name, mode = None, args = [], data = [], tags = []):
    sh_test(
        name = name,
        srcs = ["//:scripts/bazel-test.sh"],
        args = [mode or name] + args,
        data = data,
        tags = tags + ["non-rust-action"],
    )

def real_harness_gate(name, script_mode, binary, data):
    non_rust_gate(
        name = name,
        mode = script_mode,
        args = ["$(rootpath %s)" % binary],
        data = data,
        tags = ["external-harness", "manual"],
    )
