#!/usr/bin/env python3
"""Mutation coverage for package-owned history final-binary contract guards."""

from __future__ import annotations

import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


REPOSITORY = (
    Path(sys.argv[1]).resolve().parent
    if len(sys.argv) == 2
    else Path(__file__).resolve().parents[2]
)
if len(sys.argv) == 2:
    sys.argv.pop()


class HistoryApplicationContractBoundaryMutations(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name) / "repository"
        shutil.copytree(
            REPOSITORY,
            self.root,
            ignore=shutil.ignore_patterns(
                ".git",
                "bazel-*",
                "target",
                "node_modules",
            ),
        )

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def reset(self) -> None:
        self.tearDown()
        self.setUp()

    def check(self, package: str) -> subprocess.CompletedProcess[str]:
        script = {
            "ingest": "check-history-ingest-application-boundary.sh",
            "read": "check-history-read-application-dependency-boundary.sh",
        }[package]
        return subprocess.run(
            [
                str(self.root / "tools/bazel" / script),
                str(self.root / "BUILD.bazel"),
            ],
            cwd=self.root,
            check=False,
            capture_output=True,
            text=True,
        )

    def mutate(self, relative: str, before: str, after: str) -> None:
        path = self.root / relative
        text = path.read_text(encoding="utf-8")
        self.assertIn(before, text)
        path.write_text(text.replace(before, after, 1), encoding="utf-8")

    def assert_rejected(self, package: str, message: str) -> None:
        result = self.check(package)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(message, result.stdout + result.stderr)

    def test_current_contracts_pass(self) -> None:
        for package in ("ingest", "read"):
            with self.subTest(package=package):
                result = self.check(package)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_uninventoried_contract_is_rejected(self) -> None:
        self.mutate(
            "crates/ctx-history-ingest-application/BUILD.bazel",
            "history_ingest_binary_contract(\n    name = \"custom_root_discovery_tests\",",
            "history_ingest_binary_contract(\n    name = \"unreviewed_contract_tests\",\n    src = \"tests/contracts/custom_root_discovery.rs\",\n)\n\nhistory_ingest_binary_contract(\n    name = \"custom_root_discovery_tests\",",
        )
        self.assert_rejected("ingest", "final-binary contract inventory drifted")

    def test_binary_as_rust_dependency_is_rejected(self) -> None:
        self.mutate(
            "crates/ctx-history-read-application/test_targets.bzl",
            "support_deps = _CONTRACT_SUPPORT_DEPS + [",
            "support_deps = _CONTRACT_SUPPORT_DEPS + [\n            \"//crates/ctx-cli:ctx\",",
        )
        self.assert_rejected("read", "must not compile against final ctx")

    def test_ctx_rust_backedge_is_rejected(self) -> None:
        self.mutate(
            "crates/ctx-history-ingest-application/test_targets.bzl",
            "support_deps = _CONTRACT_SUPPORT_DEPS,",
            "support_deps = _CONTRACT_SUPPORT_DEPS + [\"//crates/ctx-cli-presentation:lib\"],",
        )
        self.assert_rejected("ingest", "retain a ctx or shared-support Rust backedge")

    def test_shared_support_rust_backedge_is_rejected(self) -> None:
        self.mutate(
            "crates/ctx-history-read-application/test_targets.bzl",
            "support_deps = _CONTRACT_SUPPORT_DEPS + [",
            "support_deps = _CONTRACT_SUPPORT_DEPS + [\n            \"//crates/ctx-cli-contract-tests:lib\",",
        )
        self.assert_rejected("read", "retain a ctx or shared-support Rust backedge")

    def test_current_read_adapter_direct_pinned_query_bypass_is_rejected(self) -> None:
        self.mutate(
            "crates/ctx-history-cli/src/source_index/show.rs",
            "const CLI_SESSION_EVENT_PAGE_ITEMS: usize =",
            """fn direct_query_bypass_for_mutation_test(
    index: &ctx_history_index::VerifiedIndex,
    request: &ctx_history_read_application::ShowEventRequest,
) {
    let _ = ctx_history_read_application::PinnedHistoryQuery::new(index, None)
        .show_event(request);
}

const CLI_SESSION_EVENT_PAGE_ITEMS: usize =""",
        )
        self.assert_rejected(
            "read", "bypasses the application-owned show or list authority"
        )

    def test_missing_current_read_adapter_is_rejected(self) -> None:
        (self.root / "crates/ctx-history-cli/src/source_index/locate.rs").unlink()
        self.assert_rejected("read", "expected history CLI query consumer is missing")

    def test_whole_package_cloc_ceiling_is_rejected(self) -> None:
        source = self.root / "crates/ctx-history-ingest-application/tests/contracts/cloc_growth.rs"
        source.write_text("\n".join("fn line_%d() {}" % index for index in range(6_090)), encoding="utf-8")
        self.assert_rejected("ingest", "exceeds its 6,089 physical CLOC ceiling")


if __name__ == "__main__":
    unittest.main()
