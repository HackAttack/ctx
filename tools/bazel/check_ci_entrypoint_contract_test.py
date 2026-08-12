#!/usr/bin/env python3
"""Mutation tests for the complete public CI entrypoint contract."""

from __future__ import annotations

from pathlib import Path
import unittest

from check_ci_entrypoint_contract import (
    CI_LINT_CONFIG,
    CLIPPY_WARNINGS,
    ContractError,
    validate_bazelrc_text,
    validate_build_text,
    validate_check_text,
)


ROOT = Path(__file__).resolve().parents[2]
BAZELRC_TEXT = (ROOT / ".bazelrc").read_text(encoding="utf-8")
BUILD_TEXT = (ROOT / "BUILD.bazel").read_text(encoding="utf-8")
CHECK_TEXT = (ROOT / "scripts/check.sh").read_text(encoding="utf-8")


class CiEntrypointContractTest(unittest.TestCase):
    def test_repository_contract_is_complete(self) -> None:
        validate_bazelrc_text(BAZELRC_TEXT)
        validate_build_text(BUILD_TEXT)
        validate_check_text(CHECK_TEXT)

    def test_ci_cannot_drop_inherited_lint(self) -> None:
        mutated = BAZELRC_TEXT.replace(f"{CI_LINT_CONFIG}\n", "", 1)
        self.assertNotEqual(mutated, BAZELRC_TEXT)
        with self.assertRaisesRegex(ContractError, "expected exactly one"):
            validate_bazelrc_text(mutated)

    def test_clippy_cannot_weaken_warnings(self) -> None:
        mutated = BAZELRC_TEXT.replace(CLIPPY_WARNINGS, CLIPPY_WARNINGS.replace("-D", "-W"))
        self.assertNotEqual(mutated, BAZELRC_TEXT)
        with self.assertRaisesRegex(ContractError, "expected exactly one"):
            validate_bazelrc_text(mutated)

    def test_ci_cannot_replace_the_workspace_with_a_target_list(self) -> None:
        mutated = CHECK_TEXT.replace(
            "run_bazel build //... --config=ci",
            "run_bazel build //crates/ctx-cli:all --config=ci",
            1,
        )
        self.assertNotEqual(mutated, CHECK_TEXT)
        with self.assertRaisesRegex(ContractError, "lint-build //..."):
            validate_check_text(mutated)

    def test_tests_cannot_reapply_the_lint_aspect(self) -> None:
        mutated = CHECK_TEXT.replace(
            "run_bazel test //:ci_tests --config=test",
            "run_bazel test //:ci_tests --config=ci",
            1,
        )
        self.assertNotEqual(mutated, CHECK_TEXT)
        with self.assertRaisesRegex(ContractError, "deterministic .* suite"):
            validate_check_text(mutated)

    def test_lint_must_finish_before_tests_start(self) -> None:
        build = "run_bazel build //... --config=ci"
        test = "run_bazel test //:ci_tests --config=test"
        mutated = CHECK_TEXT.replace(f"{build}\n    {test}", f"{test}\n    {build}", 1)
        self.assertNotEqual(mutated, CHECK_TEXT)
        with self.assertRaisesRegex(ContractError, "lint-build //..."):
            validate_check_text(mutated)

    def test_ambiguous_suite_alias_is_rejected(self) -> None:
        mutated = BUILD_TEXT.replace('name = "ci_tests"', 'name = "ci"', 1)
        self.assertNotEqual(mutated, BUILD_TEXT)
        with self.assertRaisesRegex(ContractError, "ambiguous root suite"):
            validate_build_text(mutated)

    def test_nightly_cannot_drop_ci(self) -> None:
        nested = '[\n        ":ci_tests",\n    ] + NIGHTLY_TESTS'
        mutated = BUILD_TEXT.replace(nested, "NIGHTLY_TESTS", 1)
        self.assertNotEqual(mutated, BUILD_TEXT)
        with self.assertRaisesRegex(ContractError, "incorrect nesting"):
            validate_build_text(mutated)


if __name__ == "__main__":
    unittest.main()
