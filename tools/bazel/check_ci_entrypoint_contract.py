#!/usr/bin/env python3
"""Validate the graph-discovered public test-mode entrypoint contract."""

from __future__ import annotations

import ast
from pathlib import Path
import sys


CI_LINT_CONFIG = "build:ci --config=lint"
CLIPPY_ASPECT = (
    "build:lint --aspects=@rules_rust//rust:defs.bzl%rust_clippy_aspect"
)
CLIPPY_OUTPUT = "build:lint --output_groups=+clippy_checks"
CLIPPY_WARNINGS = (
    "build:lint --@rules_rust//rust/settings:clippy_flag=-Dwarnings"
)


class ContractError(ValueError):
    """The public CI entrypoint is incomplete or internally inconsistent."""


def _active_lines(text: str) -> list[str]:
    return [
        line.strip()
        for line in text.splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    ]


def validate_bazelrc_text(bazelrc_text: str) -> None:
    lines = _active_lines(bazelrc_text)
    for required in (CI_LINT_CONFIG, CLIPPY_ASPECT, CLIPPY_OUTPUT, CLIPPY_WARNINGS):
        if lines.count(required) != 1:
            raise ContractError(f"expected exactly one {required!r}")

    ci_lines = [line for line in lines if line.startswith("build:ci ")]
    if any(
        "rust_clippy_aspect" in line
        or "clippy_checks" in line
        or "clippy_flag" in line
        for line in ci_lines
    ):
        raise ContractError("build:ci must inherit, not duplicate, the lint settings")

    clippy_flags = [line for line in lines if "clippy_flag=" in line]
    if clippy_flags != [CLIPPY_WARNINGS]:
        raise ContractError("the lint config must have exactly one -Dwarnings flag")


def validate_check_text(check_text: str) -> None:
    marker = 'case "${mode}" in'
    start = check_text.rfind(marker)
    if start < 0:
        raise ContractError("scripts/check.sh is missing its mode execution case")
    execution = _active_lines(check_text[start:])
    expected = [
        marker,
        "ci)",
        "run_bazel build //... --config=ci",
        "run_bazel test //... --config=test --test_tag_filters=-manual,-tier-nightly,-tier-release",
        ";;",
        "nightly)",
        "run_bazel build //... --config=ci",
        "run_bazel test //... --config=test --test_tag_filters=-manual,-tier-release",
        ";;",
        "release)",
        "run_bazel build //... --config=ci",
        "run_bazel test //... --config=test --test_tag_filters=-manual",
        ";;",
        "esac",
    ]
    if execution != expected:
        raise ContractError(
            "named modes must lint-build //... with --config=ci, then discover "
            "tests from //... with the exact default-CI exception filters"
        )


def _call_name(call: ast.Call) -> str | None:
    for keyword in call.keywords:
        if keyword.arg == "name":
            try:
                value = ast.literal_eval(keyword.value)
            except (TypeError, ValueError):
                return None
            return value if isinstance(value, str) else None
    return None


def validate_build_text(build_text: str) -> None:
    try:
        tree = ast.parse(build_text, filename="BUILD.bazel")
    except SyntaxError as error:
        raise ContractError(f"BUILD.bazel cannot be parsed: {error}") from error

    retired = {
        "ci",
        "nightly",
        "release",
        "ci_tests",
        "nightly_tests",
        "release_tests",
    }
    for node in ast.walk(tree):
        if isinstance(node, ast.Call) and _call_name(node) in retired:
            raise ContractError("retired exhaustive root test suite remains")

    policy_checks = 0
    for node in tree.body:
        if not (
            isinstance(node, ast.Expr)
            and isinstance(node.value, ast.Call)
            and isinstance(node.value.func, ast.Name)
            and node.value.func.id == "sh_test"
        ):
            continue
        if _call_name(node.value) == "test_taxonomy_policy_check":
            policy_checks += 1
    if policy_checks != 1:
        raise ContractError("exactly one live test taxonomy policy check is required")

    retired_inventories = {
        node.id
        for node in ast.walk(tree)
        if isinstance(node, ast.Name) and node.id in {"CI_TESTS", "NIGHTLY_TESTS"}
    }
    if retired_inventories:
        raise ContractError(
            f"retired exhaustive test inventories remain: {sorted(retired_inventories)}"
        )


def main() -> int:
    if len(sys.argv) != 4:
        print(
            "usage: check_ci_entrypoint_contract.py .bazelrc BUILD.bazel "
            "scripts/check.sh",
            file=sys.stderr,
        )
        return 2
    try:
        validate_bazelrc_text(Path(sys.argv[1]).read_text(encoding="utf-8"))
        validate_build_text(Path(sys.argv[2]).read_text(encoding="utf-8"))
        validate_check_text(Path(sys.argv[3]).read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, ContractError) as error:
        print(f"public CI entrypoint contract failed: {error}", file=sys.stderr)
        return 1
    print("public CI entrypoint contract: OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
