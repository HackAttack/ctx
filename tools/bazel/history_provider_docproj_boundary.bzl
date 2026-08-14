"""Root-package wiring for the document-projection provider boundary."""

load("@rules_python//python:defs.bzl", "py_test")


def history_provider_docproj_boundary_checks():
    """Declares the document-projection dependency and mutation checks."""
    py_test(
        name = "history_provider_docproj_dependency_boundary_check",
        srcs = ["//tools/bazel:check_history_provider_docproj_boundary.py"],
        main = "//tools/bazel:check_history_provider_docproj_boundary.py",
        args = [
            "$(rootpath //crates/ctx-history-provider-docproj:Cargo.toml)",
            "$(rootpath //crates/ctx-history-provider-docproj:BUILD.bazel)",
            "$(rootpath //crates/ctx-history-capture:Cargo.toml)",
            "$(rootpath //crates/ctx-history-capture:BUILD.bazel)",
            "$(rootpath //crates/ctx-history-capture:src/provider/providers/mod.rs)",
            "$(rootpath //crates/ctx-history-capture-composition:src/source_backed/registration/families/document.rs)",
            "$(rootpath //crates/ctx-history-capture-composition:src/source_backed/registration/families/event_file.rs)",
        ],
        data = [
            "//crates/ctx-history-provider-docproj:BUILD.bazel",
            "//crates/ctx-history-provider-docproj:Cargo.toml",
            "//crates/ctx-history-provider-docproj:cargo_package_data",
            "//crates/ctx-history-capture:BUILD.bazel",
            "//crates/ctx-history-capture:Cargo.toml",
            "//crates/ctx-history-capture:src/provider/providers/mod.rs",
            "//crates/ctx-history-capture-composition:src/source_backed/registration/families/document.rs",
            "//crates/ctx-history-capture-composition:src/source_backed/registration/families/event_file.rs",
        ],
    )

    py_test(
        name = "history_provider_docproj_boundary_mutation_tests",
        srcs = [
            "//tools/bazel:check_history_provider_docproj_boundary.py",
            "//tools/bazel:check_history_provider_docproj_boundary_test.py",
        ],
        imports = ["tools/bazel"],
        main = "//tools/bazel:check_history_provider_docproj_boundary_test.py",
    )
