"""Root-package wiring for history source dependency boundaries."""

load("@rules_python//python:defs.bzl", "py_test")
load("@rules_shell//shell:sh_test.bzl", "sh_test")

def history_source_boundaries():
    """Declares history source and provider-runtime boundary gates."""
    sh_test(
        name = "history_source_discovery_dependency_boundary_check",
        srcs = ["//tools/bazel:check-history-source-discovery-boundary.sh"],
        args = ["$(rootpath BUILD.bazel)"],
        data = [
            "BUILD.bazel",
            "Cargo.toml",
            "scripts/bazelw",
            "//crates/ctx-daemon-application:supervisor_discovery_environment_policy_source",
            "//crates/ctx-history-capture-model:BUILD.bazel",
            "//crates/ctx-history-capture-model:cargo_package_data",
            "//crates/ctx-history-capture-runtime:BUILD.bazel",
            "//crates/ctx-history-capture-runtime:cargo_package_data",
            "//crates/ctx-history-core:BUILD.bazel",
            "//crates/ctx-history-core:cargo_package_data",
            "//crates/ctx-history-source-discovery:BUILD.bazel",
            "//crates/ctx-history-source-discovery:canonical_discovery_environment_policy_source",
            "//crates/ctx-history-source-discovery:cargo_package_data",
            "//crates/ctx-history-source-io:BUILD.bazel",
            "//crates/ctx-history-source-io:cargo_package_data",
            "//crates/ctx-history-source-sqlite:BUILD.bazel",
            "//crates/ctx-history-source-sqlite:cargo_package_data",
            "//tools/bazel:check_history_source_discovery_boundary.py",
            "//tools/bazel:check_history_source_sqlite_boundary.py",
        ],
        tags = [
            "build-graph",
            "exclusive",
            "no-cache",
            "no-sandbox",
        ],
    )

    py_test(
        name = "history_provider_trae_dependency_boundary_check",
        srcs = ["//tools/bazel:check_history_provider_trae_boundary.py"],
        main = "//tools/bazel:check_history_provider_trae_boundary.py",
        args = [
            "$(rootpath //crates/ctx-history-provider-trae:Cargo.toml)",
            "$(rootpath //crates/ctx-history-provider-trae:BUILD.bazel)",
            "$(rootpath //crates/ctx-history-provider-trae:src/lib.rs)",
            "$(rootpath //crates/ctx-history-capture:src/provider/providers/trae.rs)",
            "$(rootpath //crates/ctx-history-capture:src/provider/source_backed/registration/families/sqlite/logical.rs)",
            "$(rootpath //crates/ctx-history-capture:src/provider_sources.rs)",
        ],
        data = [
            "//crates/ctx-history-provider-trae:BUILD.bazel",
            "//crates/ctx-history-provider-trae:Cargo.toml",
            "//crates/ctx-history-provider-trae:cargo_package_data",
            "//crates/ctx-history-provider-trae:src/lib.rs",
            "//crates/ctx-history-capture:src/provider/providers/trae.rs",
            "//crates/ctx-history-capture:src/provider/source_backed/registration/families/sqlite/logical.rs",
            "//crates/ctx-history-capture:src/provider_sources.rs",
        ],
    )

    py_test(
        name = "history_capture_runtime_dependency_boundary_check",
        srcs = ["//tools/bazel:check_history_capture_runtime_boundary.py"],
        main = "//tools/bazel:check_history_capture_runtime_boundary.py",
        args = [
            "$(rootpath Cargo.toml)",
            "$(rootpath //crates/ctx-history-capture-runtime:Cargo.toml)",
            "$(rootpath //crates/ctx-history-capture-runtime:BUILD.bazel)",
            "$(rootpath //crates/ctx-history-jsonl:Cargo.toml)",
            "$(rootpath //crates/ctx-history-jsonl:BUILD.bazel)",
        ],
        data = [
            "Cargo.toml",
            "//crates/ctx-history-capture-runtime:BUILD.bazel",
            "//crates/ctx-history-capture-runtime:Cargo.toml",
            "//crates/ctx-history-jsonl:BUILD.bazel",
            "//crates/ctx-history-jsonl:Cargo.toml",
        ],
    )

    py_test(
        name = "history_capture_runtime_boundary_mutation_tests",
        srcs = [
            "//tools/bazel:check_history_capture_runtime_boundary.py",
            "//tools/bazel:check_history_capture_runtime_boundary_test.py",
        ],
        imports = ["tools/bazel"],
        main = "//tools/bazel:check_history_capture_runtime_boundary_test.py",
    )

    py_test(
        name = "history_provider_runtime_dependency_boundary_check",
        srcs = ["//tools/bazel:check_history_provider_runtime_boundary.py"],
        main = "//tools/bazel:check_history_provider_runtime_boundary.py",
        args = [
            "$(rootpath //crates/ctx-history-provider-runtime:Cargo.toml)",
            "$(rootpath //crates/ctx-history-provider-runtime:BUILD.bazel)",
            "$(rootpath //crates/ctx-history-provider-runtime:src/adapter.rs)",
            "$(rootpath //crates/ctx-history-provider-runtime:src/error.rs)",
            "$(rootpath //crates/ctx-history-provider-runtime:src/lib.rs)",
            "$(rootpath //crates/ctx-history-provider-runtime:src/jsonl.rs)",
            "$(rootpath //crates/ctx-history-provider-runtime:src/record.rs)",
            "$(rootpath //crates/ctx-history-provider-runtime:src/route.rs)",
            "$(rootpath //crates/ctx-history-provider-runtime:src/source_io.rs)",
            "$(rootpath //crates/ctx-history-provider-runtime:src/sqlite.rs)",
            "$(rootpath //crates/ctx-history-provider-runtime:tests/provider_pack_jsonl_compile.rs)",
            "$(rootpath //crates/ctx-history-providers-jsonl-shared:src/error.rs)",
            "$(rootpath //crates/ctx-history-capture:src/provider/source_backed/family.rs)",
            "$(rootpath //crates/ctx-history-capture:src/provider/source_backed.rs)",
            "$(rootpath //crates/ctx-history-capture:src/provider/source_backed/family/jsonl_compat.rs)",
            "$(rootpath //crates/ctx-history-source-sqlite:src/lib.rs)",
            "$(rootpath //crates/ctx-history-source-sqlite:src/value.rs)",
            "$(rootpath //crates/ctx-history-capture:src/native_source.rs)",
        ],
        data = [
            "//crates/ctx-history-provider-runtime:BUILD.bazel",
            "//crates/ctx-history-provider-runtime:Cargo.toml",
            "//crates/ctx-history-provider-runtime:cargo_package_data",
            "//crates/ctx-history-provider-runtime:src/adapter.rs",
            "//crates/ctx-history-provider-runtime:src/error.rs",
            "//crates/ctx-history-provider-runtime:src/lib.rs",
            "//crates/ctx-history-provider-runtime:src/jsonl.rs",
            "//crates/ctx-history-provider-runtime:src/record.rs",
            "//crates/ctx-history-provider-runtime:src/route.rs",
            "//crates/ctx-history-provider-runtime:src/source_io.rs",
            "//crates/ctx-history-provider-runtime:src/sqlite.rs",
            "//crates/ctx-history-provider-runtime:tests/provider_pack_jsonl_compile.rs",
            "//crates/ctx-history-providers-jsonl-shared:src/error.rs",
            "//crates/ctx-history-capture:src/provider/source_backed/family.rs",
            "//crates/ctx-history-capture:src/provider/source_backed.rs",
            "//crates/ctx-history-capture:src/provider/source_backed/family/jsonl_compat.rs",
            "//crates/ctx-history-source-sqlite:src/lib.rs",
            "//crates/ctx-history-source-sqlite:src/value.rs",
            "//crates/ctx-history-capture:src/native_source.rs",
        ],
    )

    py_test(
        name = "history_provider_runtime_boundary_mutation_tests",
        srcs = [
            "//tools/bazel:check_history_provider_runtime_boundary.py",
            "//tools/bazel:check_history_provider_runtime_boundary_test.py",
        ],
        imports = ["tools/bazel"],
        main = "//tools/bazel:check_history_provider_runtime_boundary_test.py",
    )

    py_test(
        name = "history_provider_sqlite_selected_dependency_boundary_check",
        srcs = ["//tools/bazel:check_history_provider_sqlite_selected_boundary.py"],
        main = "//tools/bazel:check_history_provider_sqlite_selected_boundary.py",
        args = [
            "$(rootpath //crates/ctx-history-providers-sqlite-selected:Cargo.toml)",
            "$(rootpath //crates/ctx-history-providers-sqlite-selected:BUILD.bazel)",
            "crates/ctx-history-providers-sqlite-selected",
            "$(rootpath //crates/ctx-history-capture:Cargo.toml)",
            "$(rootpath //crates/ctx-history-capture:BUILD.bazel)",
            "crates/ctx-history-capture",
        ],
        data = [
            "//crates/ctx-history-capture:BUILD.bazel",
            "//crates/ctx-history-capture:Cargo.toml",
            "//crates/ctx-history-capture:cargo_package_data",
            "//crates/ctx-history-providers-sqlite-selected:BUILD.bazel",
            "//crates/ctx-history-providers-sqlite-selected:Cargo.toml",
            "//crates/ctx-history-providers-sqlite-selected:cargo_package_data",
        ],
    )

    py_test(
        name = "history_provider_sqlite_selected_boundary_mutation_tests",
        srcs = [
            "//tools/bazel:check_history_provider_sqlite_selected_boundary.py",
            "//tools/bazel:check_history_provider_sqlite_selected_boundary_test.py",
        ],
        imports = ["tools/bazel"],
        main = "//tools/bazel:check_history_provider_sqlite_selected_boundary_test.py",
    )
