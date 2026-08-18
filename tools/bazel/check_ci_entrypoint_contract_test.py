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
            "run_bazel test //... --config=test",
            "run_bazel test //... --config=ci",
            1,
        )
        self.assertNotEqual(mutated, CHECK_TEXT)
        with self.assertRaisesRegex(ContractError, "discover tests"):
            validate_check_text(mutated)

    def test_lint_must_finish_before_tests_start(self) -> None:
        build = "run_bazel build //... --config=ci"
        test = "run_bazel test //... --config=test --test_tag_filters=-manual,-tier-nightly,-tier-release"
        mutated = CHECK_TEXT.replace(f"{build}\n    {test}", f"{test}\n    {build}", 1)
        self.assertNotEqual(mutated, CHECK_TEXT)
        with self.assertRaisesRegex(ContractError, "lint-build //..."):
            validate_check_text(mutated)

    def test_exhaustive_suite_alias_is_rejected(self) -> None:
        mutated = BUILD_TEXT + '\ntest_suite(name = "ci_tests", tests = [])\n'
        self.assertNotEqual(mutated, BUILD_TEXT)
        with self.assertRaisesRegex(ContractError, "exhaustive root test suite"):
            validate_build_text(mutated)

    def test_ci_cannot_drop_default_exception_filter(self) -> None:
        mutated = CHECK_TEXT.replace(
            "-manual,-tier-nightly,-tier-release",
            "-manual,-tier-release",
            1,
        )
        self.assertNotEqual(mutated, CHECK_TEXT)
        with self.assertRaisesRegex(ContractError, "exception filters"):
            validate_check_text(mutated)

    def test_live_taxonomy_policy_check_is_required(self) -> None:
        mutated = BUILD_TEXT.replace(
            'name = "test_taxonomy_policy_check"',
            'name = "removed_taxonomy_policy_check"',
            1,
        )
        self.assertNotEqual(mutated, BUILD_TEXT)
        with self.assertRaisesRegex(ContractError, "taxonomy policy check"):
            validate_build_text(mutated)


if __name__ == "__main__":
    unittest.main()
