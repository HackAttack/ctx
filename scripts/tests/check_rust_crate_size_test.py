#!/usr/bin/env python3
"""Focused tests for the physical Cargo-package CLOC gate."""

from __future__ import annotations

import ast
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
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

    def test_package_internal_target_and_node_modules_are_counted(self) -> None:
        self.fixture.workspace(["crates/pkg"])
        self.fixture.package("crates/pkg", "pkg")
        self.fixture.write("crates/pkg/target/generated.rs", "fn generated() {}\n")
        self.fixture.write("crates/pkg/node_modules/vendor.rs", "fn vendored() {}\n")

        measured = gate.live_measurements(self.fixture.root)

        self.assertEqual((measured[0].files, measured[0].cloc), (3, 3))

    def test_package_internal_cache_and_vcs_named_directories_are_counted(self) -> None:
        self.fixture.workspace(["crates/pkg"])
        self.fixture.package("crates/pkg", "pkg")
        names = (
            ".git",
            ".hg",
            ".buildkite-cache",
            ".mypy_cache",
            ".pytest_cache",
            ".ruff_cache",
            ".svn",
            "__pycache__",
        )
        for index, name in enumerate(names):
            self.fixture.write(f"crates/pkg/{name}/hidden_{index}.rs", "fn hidden() {}\n")

        measured = gate.live_measurements(self.fixture.root)

        self.assertEqual((measured[0].files, measured[0].cloc), (1 + len(names), 1 + len(names)))

    def test_hidden_manifest_in_package_artifact_directory_is_rejected(self) -> None:
        self.fixture.workspace(["crates/pkg"])
        self.fixture.package("crates/pkg", "pkg")
        self.fixture.write(
            "crates/pkg/target/hidden/Cargo.toml",
            '[package]\nname = "hidden"\nversion = "0.0.0"\n',
        )

        with self.assertRaisesRegex(
            gate.GateError,
            r"undeclared Cargo\.toml.*crates/pkg/target/hidden/Cargo\.toml",
        ):
            gate.live_measurements(self.fixture.root)

    def test_package_internal_directory_symlinks_are_rejected_regardless_of_name(self) -> None:
        if not hasattr(os, "symlink"):
            self.skipTest("symlinks unavailable")
        for name in (
            "ordinary",
            "target",
            "node_modules",
            ".buildkite-cache",
            ".git",
            ".pytest_cache",
        ):
            with self.subTest(name=name):
                fixture = CheckoutFixture()
                try:
                    fixture.workspace(["crates/pkg"])
                    fixture.package("crates/pkg", "pkg")
                    target = fixture.root / "outside"
                    target.mkdir()
                    os.symlink(target, fixture.root / "crates/pkg" / name)
                    expected = re.escape(f"crates/pkg/{name}")

                    with self.assertRaisesRegex(
                        gate.GateError,
                        rf"symlinked package directory.*{expected}",
                    ):
                        gate.live_measurements(fixture.root)
                finally:
                    fixture.close()

    def test_checkout_level_artifact_directories_remain_pruned(self) -> None:
        self.fixture.workspace(["crates/pkg"])
        self.fixture.package("crates/pkg", "pkg")
        names = (*sorted(gate.EXCLUDED_DIRECTORY_NAMES), ".buildkite-cache", "bazel-out")
        for index, name in enumerate(names):
            self.fixture.write(f"{name}/ignored_{index}.rs", "fn generated() {}\n")
            self.fixture.write(
                f"{name}/hidden-{index}/Cargo.toml",
                '[package]\nname = "ignored"\nversion = "0.0.0"\n',
            )

        measured = gate.live_measurements(self.fixture.root)

        self.assertEqual((measured[0].files, measured[0].cloc), (1, 1))

    def test_checkout_level_buildkite_cache_symlink_is_pruned(self) -> None:
        if not hasattr(os, "symlink"):
            self.skipTest("symlinks unavailable")
        self.fixture.workspace(["crates/pkg"])
        self.fixture.package("crates/pkg", "pkg")
        with tempfile.TemporaryDirectory() as cache_name:
            cache = Path(cache_name)
            (cache / "hidden.rs").write_text("fn cached() {}\n", encoding="utf-8")
            (cache / "Cargo.toml").write_text(
                '[package]\nname = "cached"\nversion = "0.0.0"\n',
                encoding="utf-8",
            )
            os.symlink(cache, self.fixture.root / ".buildkite-cache")

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

    def test_retirement_is_allowed_only_at_hard_limit_or_after_removal(self) -> None:
        previous = policy()
        candidate = {**policy(), "offenders": []}
        self.assertEqual(
            gate.validate_policy_transition(candidate, previous, "b" * 40, [measurement(20_000)]),
            {},
        )
        self.assertEqual(gate.validate_policy_transition(candidate, previous, "b" * 40, []), {})

        with self.assertRaisesRegex(gate.GateError, r"entry removal forbidden.*count=20001"):
            gate.validate_policy_transition(candidate, previous, "b" * 40, [measurement(20_001)])

    def test_updater_removes_retired_entries(self) -> None:
        previous = policy()
        candidate = policy()
        updated = gate.updated_policy(candidate, previous, "b" * 40, [measurement(20_000)])
        self.assertEqual(updated["offenders"], [])
        updated = gate.updated_policy(candidate, previous, "b" * 40, [])
        self.assertEqual(updated["offenders"], [])

    def test_manifest_rename_after_split_has_no_false_tombstone(self) -> None:
        previous = policy()
        candidate = policy()
        split = measurement(19_000, root="crates/split-big")

        updated = gate.updated_policy(candidate, previous, "b" * 40, [split])

        self.assertEqual(updated["offenders"], [])
        self.assertEqual(gate.measurement_failures([split], {}), [])

    def test_retired_offender_cannot_be_resurrected(self) -> None:
        previous = {**policy(), "offenders": []}
        candidate = {**policy(), "offenders": []}
        entries = gate.validate_policy_transition(candidate, previous, "b" * 40, [measurement(20_001)])
        failures = gate.measurement_failures([measurement(20_001)], entries)
        self.assertIn("new offender forbidden", failures[0])

        with self.assertRaisesRegex(gate.GateError, "new offender entries are forbidden"):
            gate.validate_policy_transition(policy(ratchet=20_001), previous, "b" * 40, [measurement(20_001)])

    def test_retired_entry_must_be_removed(self) -> None:
        entries = gate.parse_policy(policy(), "candidate")
        failures = gate.measurement_failures([measurement(19_000)], entries)
        self.assertIn("retired offender entry must be removed", failures[0])
        self.assertIn("retired offender entry must be removed", gate.measurement_failures([], entries)[0])

    def test_stale_ratchet_reports_atomic_update_command_once(self) -> None:
        entries = gate.parse_policy(policy(), "candidate")
        message = gate.format_failures(gate.measurement_failures([measurement(24_000)], entries))

        self.assertIn("package=big count=24000 limit=20000 ratchet=25000", message)
        self.assertIn("stale ratchet after shrink", message)
        self.assertEqual(message.count(gate.UPDATE_COMMAND), 1)

    def test_atomic_update_shrinks_but_cannot_raise_accepted_ratchet(self) -> None:
        previous = policy()
        candidate = policy()
        updated = gate.updated_policy(candidate, previous, "b" * 40, [measurement(24_000)])
        self.assertEqual(updated["offenders"][0]["ratchet"], 24_000)

        with self.assertRaisesRegex(gate.GateError, "ratchet raise forbidden"):
            gate.updated_policy(candidate, previous, "b" * 40, [measurement(25_001)])

    def test_extra_or_malformed_policy_fields_are_rejected(self) -> None:
        extra = policy()
        extra["unexpected"] = True
        with self.assertRaisesRegex(gate.GateError, "schema is unsupported"):
            gate.parse_policy(extra, "candidate")

        malformed = policy()
        malformed["offenders"][0]["reason"] = "manual exception"
        with self.assertRaisesRegex(gate.GateError, "offender entry is malformed"):
            gate.parse_policy(malformed, "candidate")

    def test_checked_policy_is_minimal_and_has_two_honest_offenders(self) -> None:
        value = json.loads((SCRIPT.parent / "check-rust-crate-size-policy-v1.json").read_text())
        entries = gate.parse_policy(value, "checked")
        self.assertEqual(
            list(entries),
            ["ctx", "ctx-history-capture"],
        )
        self.assertEqual(value["hard_limit"], 20_000)
        self.assertEqual(value["metric"], gate.METRIC)
        self.assertTrue(all(set(entry) == {"package", "manifest", "ratchet"} for entry in value["offenders"]))


class TemporaryGitCheckout:
    def __init__(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.run("init", "-q", "-b", "main")
        self.run("config", "user.email", "crate-gate@example.invalid")
        self.run("config", "user.name", "Crate Gate Test")
        self.run("config", "commit.gpgsign", "false")

    def close(self) -> None:
        self.temporary.cleanup()

    def run(self, *arguments: str) -> str:
        result = subprocess.run(
            ["git", *arguments],
            cwd=self.root,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            text=True,
        )
        if result.returncode:
            raise AssertionError(f"git {' '.join(arguments)} failed: {result.stderr}")
        return result.stdout.strip()

    def write(self, relative: str, content: str) -> None:
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")

    def commit(self, message: str) -> str:
        self.run("add", "-A")
        self.run("commit", "-q", "-m", message)
        return self.run("rev-parse", "HEAD")

    def base_commit(self) -> str:
        self.write(gate.POLICY_PATH, gate.canonical_json(policy()).decode())
        self.write("marker", "base\n")
        return self.commit("base")


class GitTransitionTests(unittest.TestCase):
    def setUp(self) -> None:
        self.checkout = TemporaryGitCheckout()

    def tearDown(self) -> None:
        self.checkout.close()

    def test_pr_head_reads_origin_main_policy(self) -> None:
        base = self.checkout.base_commit()
        self.checkout.run("update-ref", "refs/remotes/origin/main", base)
        self.checkout.run("switch", "-q", "-c", "candidate")
        self.checkout.write("marker", "candidate\n")
        self.checkout.commit("candidate")

        selected, previous = gate.previous_accepted_policy(self.checkout.root)

        self.assertEqual(selected, base)
        self.assertEqual(previous, policy())

    def test_post_merge_head_equal_origin_reads_first_parent_policy(self) -> None:
        base = self.checkout.base_commit()
        self.checkout.run("switch", "-q", "-c", "pr")
        self.checkout.write(gate.POLICY_PATH, gate.canonical_json(policy(ratchet=24_000)).decode())
        self.checkout.commit("candidate")
        self.checkout.run("switch", "-q", "main")
        self.checkout.run("merge", "-q", "--no-ff", "pr", "-m", "merge result")
        merge = self.checkout.run("rev-parse", "HEAD")
        self.checkout.run("update-ref", "refs/remotes/origin/main", merge)

        selected, previous = gate.previous_accepted_policy(self.checkout.root)

        self.assertEqual(selected, base)
        self.assertEqual(previous, policy())

    def test_origin_main_advanced_beyond_pr_base_fails_closed(self) -> None:
        base = self.checkout.base_commit()
        self.checkout.run("switch", "-q", "-c", "candidate")
        self.checkout.write("candidate", "candidate\n")
        self.checkout.commit("candidate")
        self.checkout.run("switch", "-q", "main")
        self.checkout.write("main", "advanced\n")
        advanced = self.checkout.commit("main advanced")
        self.checkout.run("update-ref", "refs/remotes/origin/main", advanced)
        self.checkout.run("switch", "-q", "candidate")

        with self.assertRaisesRegex(gate.GateError, "origin/main advanced beyond candidate base"):
            gate.previous_accepted_policy(self.checkout.root)

        self.assertNotEqual(base, advanced)

    def test_stale_origin_main_relative_to_local_main_fails_closed(self) -> None:
        base = self.checkout.base_commit()
        self.checkout.run("update-ref", "refs/remotes/origin/main", base)
        self.checkout.write("main", "advanced\n")
        self.checkout.commit("local main advanced")
        self.checkout.run("switch", "-q", "-c", "candidate")
        self.checkout.write("candidate", "candidate\n")
        self.checkout.commit("candidate")

        with self.assertRaisesRegex(gate.GateError, "origin/main is stale relative to local main"):
            gate.previous_accepted_policy(self.checkout.root)

    def test_missing_origin_main_fails_closed(self) -> None:
        self.checkout.base_commit()

        with self.assertRaisesRegex(gate.GateError, "refs/remotes/origin/main"):
            gate.previous_accepted_policy(self.checkout.root)


class PythonCompatibilityTests(unittest.TestCase):
    def test_checker_uses_python_310_syntax_and_declared_tomli(self) -> None:
        source = SCRIPT.read_text(encoding="utf-8")
        tree = ast.parse(source, filename=str(SCRIPT), feature_version=(3, 10))
        toml_imports = [
            (alias.name, alias.asname)
            for node in ast.walk(tree)
            if isinstance(node, ast.Import)
            for alias in node.names
            if alias.name in {"tomli", "tomllib"}
        ]
        self.assertEqual(toml_imports, [("tomli", "tomllib")])
        self.assertNotIn("sys.version_info", source)


if __name__ == "__main__":
    unittest.main()
