"""Root-package wiring for the logical SQLite provider boundary."""

load("@rules_python//python:defs.bzl", "py_test")
load("@rules_shell//shell:sh_test.bzl", "sh_test")


def history_provider_sqlite_logical_boundary_checks():
    """Declares the logical SQLite dependency and mutation checks."""
    sh_test(
        name = "history_provider_sqlite_logical_dependency_boundary_check",
        srcs = ["//tools/bazel:check-history-provider-sqlite-logical-boundary.sh"],
        args = ["$(rootpath BUILD.bazel)"],
        data = [
            "BUILD.bazel",
            "Cargo.toml",
            "scripts/bazelw",
            "//crates/ctx-history-capture-composition:BUILD.bazel",
            "//crates/ctx-history-capture-composition:cargo_package_data",
            "//crates/ctx-history-capture-model:BUILD.bazel",
            "//crates/ctx-history-capture-model:cargo_package_data",
            "//crates/ctx-history-capture-runtime:BUILD.bazel",
            "//crates/ctx-history-capture-runtime:cargo_package_data",
            "//crates/ctx-history-core:BUILD.bazel",
            "//crates/ctx-history-core:cargo_package_data",
            "//crates/ctx-history-providers-sqlite-logical:BUILD.bazel",
            "//crates/ctx-history-providers-sqlite-logical:cargo_package_data",
            "//crates/ctx-history-source-discovery:BUILD.bazel",
            "//crates/ctx-history-source-discovery:cargo_package_data",
            "//crates/ctx-history-source-io:BUILD.bazel",
            "//crates/ctx-history-source-io:cargo_package_data",
            "//crates/ctx-history-source-sqlite:BUILD.bazel",
            "//crates/ctx-history-source-sqlite:cargo_package_data",
            "//tools/bazel:check_history_provider_sqlite_logical_boundary.py",
        ],
        tags = [
            "build-graph",
            "exclusive",
            "no-cache",
            "no-sandbox",
        ],
    )

    py_test(
        name = "history_provider_sqlite_logical_boundary_mutation_tests",
        srcs = [
            "//tools/bazel:check_history_provider_sqlite_logical_boundary.py",
            "//tools/bazel:check_history_provider_sqlite_logical_boundary_test.py",
        ],
        imports = ["tools/bazel"],
        main = "//tools/bazel:check_history_provider_sqlite_logical_boundary_test.py",
    )
