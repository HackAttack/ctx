#!/usr/bin/env python3
"""Focused tests for the physical Cargo-package CLOC gate."""

from __future__ import annotations

import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "check-rust-crate-size.py"
SPEC = importlib.util.spec_from_file_location("check_rust_crate_size", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
gate = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = gate
SPEC.loader.exec_module(gate)


class CheckoutFixture:
    def __init__(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)

    def close(self) -> None:
        self.temporary.cleanup()

    def write(self, relative: str, content: str) -> Path:
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")
        return path

    def workspace(self, members: list[str]) -> None:
        rendered = ", ".join(json.dumps(member) for member in members)
        self.write("Cargo.toml", f"[workspace]\nmembers = [{rendered}]\n")

    def package(self, root: str, name: str, source: str = "fn main() {}\n") -> None:
        self.write(
            f"{root}/Cargo.toml",
            f'[package]\nname = "{name}"\nversion = "0.0.0"\nedition = "2021"\n',
        )
        self.write(f"{root}/src/lib.rs", source)


def package(name: str = "big", root: str = "crates/big") -> gate.Package:
    return gate.Package(name=name, manifest=f"{root}/Cargo.toml", root=root)


def measurement(count: int, name: str = "big", root: str = "crates/big") -> gate.Measurement:
    return gate.Measurement(package=package(name, root), cloc=count, files=1)


def policy(
    *,
    ratchet: int = 25_000,
    admission: int = 25_000,
    admission_sha: str = "a" * 40,
    name: str = "big",
    root: str = "crates/big",
) -> dict[str, object]:
    return {
        "schema_version": 1,
        "metric": gate.METRIC,
        "hard_limit": gate.HARD_LIMIT,
        "admission_sha": admission_sha,
        "offenders": [
            {
                "package": name,
                "manifest": f"{root}/Cargo.toml",
                "admission_cloc": admission,
                "ratchet": ratchet,
            }
        ],
    }


class PhysicalCensusTests(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = CheckoutFixture()

    def tearDown(self) -> None:
        self.fixture.close()

    def test_counts_every_rust_file_beneath_package_root(self) -> None:
        self.fixture.workspace(["crates/pkg"])
        self.fixture.package("crates/pkg", "pkg")
        for path in (
            "crates/pkg/build.rs",
            "crates/pkg/src/tests.rs",
            "crates/pkg/tests/integration.rs",
            "crates/pkg/examples/demo.rs",
            "crates/pkg/benches/bench.rs",
            "crates/pkg/dead/conditional.rs",
        ):
            self.fixture.write(path, "// comment\nfn counted() {}\n")

        measured = gate.live_measurements(self.fixture.root)

        self.assertEqual([(item.package.name, item.files, item.cloc) for item in measured], [("pkg", 7, 7)])

    def test_untracked_rust_file_is_seen_without_git(self) -> None:
        self.fixture.workspace(["crates/pkg"])
        self.fixture.package("crates/pkg", "pkg")
        self.fixture.write("crates/pkg/scratch/untracked.rs", "fn untracked() {}\n")

        measured = gate.live_measurements(self.fixture.root)

        self.assertEqual(measured[0].files, 2)
        self.assertEqual(measured[0].cloc, 2)

    def test_orphan_rust_file_is_rejected(self) -> None:
        self.fixture.workspace(["crates/pkg"])
        self.fixture.package("crates/pkg", "pkg")
        self.fixture.write("scratch/orphan.rs", "fn orphan() {}\n")

        with self.assertRaisesRegex(gate.GateError, r"exactly one workspace package: scratch/orphan\.rs"):
            gate.live_measurements(self.fixture.root)

    def test_undeclared_manifest_is_rejected(self) -> None:
        self.fixture.workspace(["crates/pkg"])
        self.fixture.package("crates/pkg", "pkg")
        self.fixture.write("other/Cargo.toml", '[package]\nname = "hidden"\nversion = "0.0.0"\n')

        with self.assertRaisesRegex(gate.GateError, r"undeclared Cargo\.toml.*other/Cargo\.toml"):
            gate.live_measurements(self.fixture.root)

    def test_nested_package_roots_are_rejected(self) -> None:
        self.fixture.workspace(["crates/pkg", "crates/pkg/nested"])
        self.fixture.package("crates/pkg", "pkg")
        self.fixture.package("crates/pkg/nested", "nested")

        with self.assertRaisesRegex(gate.GateError, "overlapping or nested workspace package roots"):
            gate.workspace_packages(self.fixture.root)

    def test_symlinked_rust_file_is_rejected(self) -> None:
        if not hasattr(os, "symlink"):
            self.skipTest("symlinks unavailable")
        self.fixture.workspace(["crates/pkg"])
        self.fixture.package("crates/pkg", "pkg")
        target = self.fixture.write("outside.fixture", "fn linked() {}\n")
        os.symlink(target, self.fixture.root / "crates/pkg/src/linked.rs")

        with self.assertRaisesRegex(gate.GateError, r"symlinked Rust file.*linked\.rs"):
            gate.live_measurements(self.fixture.root)

    def test_known_output_directories_are_not_package_sources(self) -> None:
        self.fixture.workspace(["crates/pkg"])
        self.fixture.package("crates/pkg", "pkg")
        self.fixture.write("crates/pkg/target/generated.rs", "fn generated() {}\n")
        self.fixture.write("target/root-generated.rs", "fn generated() {}\n")

        measured = gate.live_measurements(self.fixture.root)

        self.assertEqual((measured[0].files, measured[0].cloc), (1, 1))


class CounterTests(unittest.TestCase):
    def test_metric_counts_code_and_ignores_comment_only_lines(self) -> None:
        content = br'''// line comment
fn one() {}
/* outer
   /* nested */
*/
let ordinary = "// text, not a comment";
let raw = r#"/* text
still string */"#;
let quote = '"';
let byte = b'/';
let lifetime: &'static str = "ok";
/* leading */ fn two() {} // trailing
'''
        self.assertEqual(gate.rust_cloc(content), 8)

    def test_metric_rejects_malformed_utf8_and_unterminated_lexemes(self) -> None:
        with self.assertRaisesRegex(gate.GateError, "not UTF-8"):
            gate.rust_cloc(b"\xff")
        with self.assertRaisesRegex(gate.GateError, "unterminated block comment"):
            gate.rust_cloc(b"/*")
        with self.assertRaisesRegex(gate.GateError, "unterminated string literal"):
            gate.rust_cloc(b'let value = "unterminated')


class PolicyTests(unittest.TestCase):
    def test_bootstrap_requires_exact_current_offenders_and_counts(self) -> None:
        candidate = policy()
        entries = gate.validate_policy_transition(candidate, None, "a" * 40, [measurement(25_000)])
        self.assertEqual(entries["big"]["ratchet"], 25_000)

        with self.assertRaisesRegex(gate.GateError, "exact current CLOC"):
            gate.validate_policy_transition(candidate, None, "a" * 40, [measurement(24_999)])

    def test_new_package_over_limit_is_rejected(self) -> None:
        failures = gate.measurement_failures([measurement(20_001, "new", "crates/new")], {})
        self.assertEqual(
            failures,
            ["package=new count=20001 limit=20000 ratchet=none new offender forbidden"],
        )

    def test_growth_above_previous_ratchet_is_rejected(self) -> None:
        failures = gate.measurement_failures([measurement(25_001)], gate.parse_policy(policy(), "candidate"))
        self.assertIn("count=25001", failures[0])
        self.assertIn("ratchet=25000", failures[0])
        self.assertIn("growth forbidden", failures[0])

    def test_two_revision_ratchet_raise_is_rejected(self) -> None:
        previous = policy(ratchet=24_000)
        candidate = policy(ratchet=24_001)

        with self.assertRaisesRegex(gate.GateError, r"ratchet raise forbidden.*previous_ratchet=24000"):
            gate.validate_policy_transition(candidate, previous, "b" * 40, [measurement(24_001)])

    def test_retired_offender_cannot_be_resurrected(self) -> None:
        previous = policy(ratchet=20_000)
        candidate = policy(ratchet=20_000)
        entries = gate.validate_policy_transition(candidate, previous, "b" * 40, [measurement(20_001)])
        failures = gate.measurement_failures([measurement(20_001)], entries)
        self.assertIn("growth forbidden", failures[0])

        raised = policy(ratchet=20_001)
        with self.assertRaisesRegex(gate.GateError, "ratchet raise forbidden"):
            gate.validate_policy_transition(raised, previous, "b" * 40, [measurement(20_001)])

    def test_stale_ratchet_reports_atomic_update_command_once(self) -> None:
        entries = gate.parse_policy(policy(), "candidate")
        message = gate.format_failures(gate.measurement_failures([measurement(24_000)], entries))

        self.assertIn("package=big count=24000 limit=20000 ratchet=25000", message)
        self.assertIn("stale ratchet after shrink", message)
        self.assertEqual(message.count(gate.UPDATE_COMMAND), 1)

    def test_atomic_update_shrinks_but_cannot_raise_accepted_ratchet(self) -> None:
        previous = policy(admission=30_000)
        candidate = policy(admission=30_000)
        updated = gate.updated_policy(candidate, previous, "b" * 40, [measurement(24_000)])
        self.assertEqual(updated["offenders"][0]["ratchet"], 24_000)

        with self.assertRaisesRegex(gate.GateError, "ratchet raise forbidden"):
            gate.updated_policy(candidate, previous, "b" * 40, [measurement(25_001)])

    def test_offender_entry_is_permanent_after_package_removal(self) -> None:
        entries = gate.parse_policy(policy(ratchet=21_000), "candidate")
        failures = gate.measurement_failures([], entries)
        self.assertIn("count=0", failures[0])
        self.assertIn("expected_ratchet=20000", failures[0])

        entries = gate.parse_policy(policy(ratchet=20_000), "candidate")
        self.assertEqual(gate.measurement_failures([], entries), [])

    def test_extra_or_malformed_policy_fields_are_rejected(self) -> None:
        extra = policy()
        extra["unexpected"] = True
        with self.assertRaisesRegex(gate.GateError, "schema is unsupported"):
            gate.parse_policy(extra, "candidate")

        malformed = policy()
        malformed["offenders"][0]["reason"] = "manual exception"
        with self.assertRaisesRegex(gate.GateError, "offender entry is malformed"):
            gate.parse_policy(malformed, "candidate")

    def test_entries_cannot_be_added_or_removed_after_admission(self) -> None:
        previous = policy()
        candidate = policy()
        candidate["offenders"] = []
        with self.assertRaisesRegex(gate.GateError, "offender entries are permanent"):
            gate.validate_policy_transition(candidate, previous, "b" * 40, [measurement(19_000)])

    def test_checked_policy_is_minimal_and_has_four_honest_offenders(self) -> None:
        value = json.loads((SCRIPT.parent / "check-rust-crate-size-policy-v1.json").read_text())
        entries = gate.parse_policy(value, "checked")
        self.assertEqual(
            list(entries),
            ["ctx", "ctx-history-capture", "ctx-history-index", "ctx-history-refresh"],
        )
        self.assertEqual(value["hard_limit"], 20_000)
        self.assertEqual(value["metric"], gate.METRIC)


if __name__ == "__main__":
    unittest.main()
