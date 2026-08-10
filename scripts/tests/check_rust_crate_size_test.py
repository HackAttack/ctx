#!/usr/bin/env python3

from __future__ import annotations

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]


def load_module(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


gate = load_module("rust_crate_size", ROOT / "scripts/check-rust-crate-size.py")
authority = load_module("rust_target_authority", ROOT / "tools/bazel/check_rust_target_inventory.py")


class Fixture:
    def __init__(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)

    def close(self) -> None:
        self.temporary.cleanup()

    def write(self, path: str, value: str = "pub fn item() {}\n") -> None:
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(value, encoding="utf-8")

    def view(self) -> gate.SourceView:
        paths = {
            path.relative_to(self.root).as_posix()
            for path in self.root.rglob("*")
            if path.is_file()
        }
        return gate.SourceView(self.root, paths)


def package(name: str = "fixture", root: str = "crate") -> dict[str, str]:
    return {
        "package": name,
        "manifest": f"{root}/Cargo.toml",
        "root": root,
        "build": f"{root}/BUILD.bazel",
    }


def measured_sources(
    view: gate.SourceView,
    *,
    packages: list[dict[str, str]] | None = None,
) -> dict[str, set[str]]:
    package_list = packages or [package()]
    return gate.production_sources(view, package_list, {})


def metadata(root: Path, *, features: dict[str, list[str]] | None = None) -> bytes:
    package_id = f"path+file://{root}/crate#fixture@0.1.0"
    return json.dumps(
        {
            "packages": [
                {
                    "name": "fixture",
                    "id": package_id,
                    "manifest_path": f"{root}/crate/Cargo.toml",
                    "targets": [
                        {
                            "kind": ["lib"],
                            "name": "fixture",
                            "src_path": f"{root}/crate/src/lib.rs",
                        },
                        {
                            "kind": ["test"],
                            "name": "integration",
                            "src_path": f"{root}/crate/tests/integration.rs",
                        },
                    ],
                    "features": features or {},
                }
            ],
            "workspace_members": [package_id],
            "workspace_root": str(root),
            "target_directory": f"{root}/target",
            "version": 1,
        }
    ).encode()


def xml_query(*body: str) -> bytes:
    return ("<query version='2'>" + "".join(body) + "</query>").encode()


def source(label: str) -> str:
    return f"<source-file name='{label}'/>"


def rust_rule(
    kind: str,
    label: str,
    sources: list[str],
    *,
    crate_root: str | None = None,
    testonly: bool = False,
) -> str:
    testonly_xml = "<boolean name='testonly' value='true'/>" if testonly else ""
    labels = "".join(f"<label value='{item}'/>" for item in sources)
    root = crate_root or sources[0]
    return (
        f"<rule class='{kind}' name='{label}'>{testonly_xml}"
        f"<list name='srcs'>{labels}</list><label name='crate_root' value='{root}'/></rule>"
    )


def package_record(name: str, manifest: str, code: int) -> dict[str, object]:
    return {"package": name, "manifest": manifest, "production_cloc": code}


def ledger_policy(*, active: list[dict[str, object]], retired: list[dict[str, object]]) -> dict[str, object]:
    return {
        "grandfathered_at": "a" * 40,
        "hard_limit": 20_000,
        "exception_ledger": {"active": active, "retired": retired},
    }


def active_exception(ceiling: int) -> dict[str, object]:
    return {
        "package": "legacy",
        "manifest": "crates/legacy/Cargo.toml",
        "maximum_cloc": ceiling,
    }


def retired_exception() -> dict[str, object]:
    return {
        "package": "legacy",
        "manifest": "crates/legacy/Cargo.toml",
        "admission_cloc": 30_000,
    }


class RustCrateSizeAuthorityTest(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = Fixture()
        self.addCleanup(self.fixture.close)

    def test_package_union_counts_cfg_feature_path_dead_inline_build_and_test_sources(self) -> None:
        self.fixture.write(
            "crate/src/lib.rs",
            "#[cfg(test)] mod inline { fn test() {} }\n"
            "#[cfg(feature = \"optional-dep\")] mod optional;\n"
            "#[cfg_attr(feature = \"alternate\", path = \"alternate.rs\")] mod selected;\n",
        )
        expected = {
            "crate/src/lib.rs",
            "crate/src/optional.rs",
            "crate/src/alternate.rs",
            "crate/src/selected.rs",
            "crate/src/test_support.rs",
            "crate/src/dead.rs",
            "crate/build.rs",
            "crate/build_helper.rs",
            "crate/tests/integration.rs",
            "crate/tests/support.rs",
            "crate/examples/example.rs",
            "crate/benches/bench.rs",
            "snapshots/crate/src/old.rs",
        }
        for path in expected - {"crate/src/lib.rs"}:
            self.fixture.write(path)

        self.assertEqual(measured_sources(self.fixture.view())["fixture"], expected)

    def test_feature_gated_production_module_also_owned_by_test_glob_counts_and_fails(self) -> None:
        self.fixture.write("crate/src/lib.rs", '#[cfg(feature = "hidden")] mod hidden;\n')
        self.fixture.write("crate/src/hidden.rs")
        self.fixture.write("crate/tests/unit.rs")
        raw = xml_query(
            source("//crate:src/lib.rs"),
            source("//crate:src/hidden.rs"),
            source("//crate:tests/unit.rs"),
            rust_rule("rust_library", "//crate:lib", ["//crate:src/lib.rs"]),
            rust_rule(
                "rust_test",
                "//crate:unit_tests",
                ["//crate:tests/unit.rs", "//crate:src/hidden.rs"],
                crate_root="//crate:tests/unit.rs",
            ),
        )
        records, _ = authority.parse_bazel_query_xml(gate, raw)
        authority.validate_graph_and_labels(gate, [package()], records, self.fixture.view().paths)
        sources = measured_sources(self.fixture.view())["fixture"]
        report = gate.build_package_report(
            [package()],
            {"fixture": sources},
            {path: 20_001 if path == "crate/src/hidden.rs" else 0 for path in sources},
            {},
        )

        self.assertIn("crate/src/hidden.rs", sources)
        self.assertEqual([item["code"] for item in gate.evaluate_limits(report, {})], ["crate_limit"])

    def test_unknown_cfg_attr_path_counts_default_and_alternate_files(self) -> None:
        self.fixture.write(
            "crate/src/lib.rs",
            '#[cfg_attr(feature = "alternate", path = "alternate.rs")] mod selected;\n',
        )
        self.fixture.write("crate/src/selected.rs")
        self.fixture.write("crate/src/alternate.rs")

        sources = measured_sources(self.fixture.view())["fixture"]

        self.assertIn("crate/src/selected.rs", sources)
        self.assertIn("crate/src/alternate.rs", sources)

    def test_testonly_library_remains_production_and_test_claim_subtracts_nothing(self) -> None:
        raw = xml_query(
            source("//crate:src/shared.rs"),
            rust_rule("rust_library", "//crate:test_support", ["//crate:src/shared.rs"], testonly=True),
            rust_rule("rust_test", "//crate:unit_tests", ["//crate:src/shared.rs"]),
        )
        records, _ = authority.parse_bazel_query_xml(gate, raw)
        production, by_label = authority.validate_graph_and_labels(
            gate, [package()], records, {"crate/src/shared.rs"}
        )

        self.assertEqual(production["fixture"], ["//crate:test_support"])
        self.assertEqual(by_label["//crate:test_support"]["kind"], "rust_library")
        self.assertEqual(by_label["//crate:unit_tests"]["kind"], "rust_test")

    def test_bazel_filegroup_overapproximates_all_select_sources(self) -> None:
        raw = xml_query(
            source("//crate:src/default.rs"),
            source("//crate:src/windows.rs"),
            "<rule class='filegroup' name='//crate:selected'><list name='srcs'>"
            "<label value='//crate:src/default.rs'/><label value='//crate:src/windows.rs'/>"
            "</list></rule>",
            rust_rule(
                "rust_library",
                "//crate:lib",
                ["//crate:selected"],
                crate_root="//crate:src/default.rs",
            ),
        )

        records, canonical = authority.parse_bazel_query_xml(gate, raw)

        self.assertEqual(
            records,
            [
                {
                    "label": "//crate:lib",
                    "kind": "rust_library",
                    "crate_root": "crate/src/default.rs",
                    "sources": ["crate/src/default.rs", "crate/src/windows.rs"],
                }
            ],
        )
        self.assertEqual(canonical, gate.canonical_bytes(records))

    def test_bazel_only_source_counts_and_orphan_target_fails(self) -> None:
        self.fixture.write("crate/src/lib.rs")
        self.fixture.write("crate/src/bazel_only.rs")
        self.assertIn("crate/src/bazel_only.rs", measured_sources(self.fixture.view())["fixture"])
        records = [
            {
                "label": "//outside:lib",
                "kind": "rust_library",
                "crate_root": "crate/src/bazel_only.rs",
                "sources": ["crate/src/bazel_only.rs"],
            }
        ]
        with self.assertRaisesRegex(gate.GateError, "outside every Cargo workspace package"):
            authority.validate_graph_and_labels(gate, [package()], records, self.fixture.view().paths)

    def test_cross_package_bazel_source_edge_fails(self) -> None:
        packages = [package("one", "one"), package("two", "two")]
        records = [
            {
                "label": "//one:lib",
                "kind": "rust_library",
                "crate_root": "one/src/lib.rs",
                "sources": ["one/src/lib.rs", "two/src/stolen.rs"],
            }
        ]
        with self.assertRaisesRegex(gate.GateError, "cross-package Bazel Rust source edge"):
            authority.validate_graph_and_labels(
                gate, packages, records, {"one/src/lib.rs", "two/src/stolen.rs"}
            )

    def test_cross_package_cargo_target_root_fails(self) -> None:
        packages = [package("one", "one"), package("two", "two")]
        target = gate.CargoTarget("lib:one", "lib", "one", "two/src/stolen.rs")
        with self.assertRaisesRegex(gate.GateError, "cross-package Cargo Rust source edge"):
            authority.validate_cargo_target_ownership(
                gate, packages, packages[0], {target.key: target}
            )

    def test_generated_external_and_unresolved_bazel_sources_fail(self) -> None:
        generated = xml_query(
            "<generated-file name='//crate:generated.rs' generating-rule='//crate:gen'/>",
            rust_rule("rust_library", "//crate:lib", ["//crate:generated.rs"]),
        )
        external = xml_query(
            rust_rule("rust_library", "//crate:lib", ["@repo//:external.rs"]),
        )

        with self.assertRaisesRegex(gate.GateError, "generated Rust source"):
            authority.parse_bazel_query_xml(gate, generated)
        with self.assertRaisesRegex(gate.GateError, "unresolved Bazel Rust source"):
            authority.parse_bazel_query_xml(gate, external)

    def test_live_owner_kind_root_mapping_is_derived_and_one_to_one(self) -> None:
        pkg = package()
        targets = {
            "lib:fixture": gate.CargoTarget("lib:fixture", "lib", "fixture", "crate/src/lib.rs"),
            "test:integration": gate.CargoTarget(
                "test:integration", "test", "integration", "crate/tests/integration.rs"
            ),
            "custom-build:build-script-build": gate.CargoTarget(
                "custom-build:build-script-build", "custom-build", "build-script-build", "crate/build.rs"
            ),
        }
        facts = {
            "//crate:lib": {
                "label": "//crate:lib",
                "owner": "fixture",
                "kind": "rust_library",
                "crate_root": "crate/src/lib.rs",
                "sources": ["crate/src/lib.rs"],
            },
            "//crate:unit": {
                "label": "//crate:unit",
                "owner": "fixture",
                "kind": "rust_test",
                "crate_root": "crate/tests/integration.rs",
                "sources": ["crate/tests/integration.rs"],
            },
            "//crate:arbitrary": {
                "label": "//crate:arbitrary",
                "owner": "fixture",
                "kind": "rust_library",
                "crate_root": "crate/src/other.rs",
                "sources": ["crate/src/other.rs"],
            },
        }

        derived = authority.derive_cargo_bazel_targets(gate, pkg, targets, facts)

        self.assertEqual(
            derived,
            {"lib:fixture": "//crate:lib", "test:integration": "//crate:unit"},
        )
        duplicate = dict(targets)
        duplicate["bin:alias"] = gate.CargoTarget("bin:alias", "bin", "alias", "crate/src/lib.rs")
        facts["//crate:lib"]["kind"] = "rust_binary"
        with self.assertRaisesRegex(gate.GateError, "derive exactly one Bazel identity"):
            authority.derive_cargo_bazel_targets(gate, pkg, duplicate, facts)

    def test_symlinked_rust_source_is_not_a_hermetic_checked_input(self) -> None:
        self.fixture.write("outside.rs")
        source_path = self.fixture.root / "crate/src/lib.rs"
        source_path.parent.mkdir(parents=True)
        source_path.symlink_to(self.fixture.root / "outside.rs")

        with self.assertRaisesRegex(gate.GateError, "declared source is unavailable"):
            gate.file_census_digest(self.fixture.view())
        declared_view = gate.SourceView(self.fixture.root, self.fixture.view().paths, allow_symlinks=True)
        self.assertIsInstance(gate.file_census_digest(declared_view), str)

    def test_cargo_metadata_is_path_canonical_and_implicit_features_cannot_hide_sources(self) -> None:
        self.fixture.write("crate/Cargo.toml", "[package]\nname='fixture'\nversion='0.1.0'\n")
        self.fixture.write("crate/src/lib.rs")
        self.fixture.write("crate/src/implicit_optional.rs")
        self.fixture.write("crate/tests/integration.rs")
        first, first_bytes = gate.canonical_cargo_metadata(
            metadata(self.fixture.root, features={"implicit-optional": ["dep:implicit-optional"]}),
            self.fixture.root,
        )
        second, second_bytes = gate.canonical_cargo_metadata(
            metadata(self.fixture.root, features={}), self.fixture.root
        )

        packages = gate.packages_from_cargo_metadata(first)

        self.assertEqual(packages[0]["root"], "crate")
        self.assertIn("test:integration", packages[0]["cargo_target_roots"])
        self.assertNotEqual(hashlib.sha256(first_bytes).digest(), hashlib.sha256(second_bytes).digest())
        self.assertIn("crate/src/implicit_optional.rs", measured_sources(self.fixture.view())["fixture"])

    def test_undeclared_live_input_and_stale_manifest_fail(self) -> None:
        declared = {"Cargo.toml", "crate/Cargo.toml", "crate/BUILD.bazel", "crate/src/lib.rs"}
        with self.assertRaisesRegex(gate.GateError, "declared/live census drift"):
            authority.validate_declared_live_census(gate, declared, declared | {"crate/src/new.rs"})
        with self.assertRaisesRegex(gate.GateError, "declared/live census drift"):
            authority.validate_declared_live_census(gate, declared | {"crate/src/stale.rs"}, declared)

    def test_git_discovery_ignores_global_excludes_and_rejects_local_excludes(self) -> None:
        subprocess.run(["git", "init", "-q"], cwd=self.fixture.root, check=True)
        self.fixture.write("crate/src/live.rs")
        self.fixture.write("global-ignore", "*.rs\n")
        self.fixture.write("global-config", f"[core]\nexcludesFile={self.fixture.root / 'global-ignore'}\n")
        with mock.patch.dict(os.environ, {"GIT_CONFIG_GLOBAL": str(self.fixture.root / "global-config")}):
            self.assertIn("crate/src/live.rs", authority.git_live_paths(gate, self.fixture.root))
        self.fixture.write(".git/info/exclude", "*.rs\n")
        with self.assertRaisesRegex(gate.GateError, "checkout-local Git info/exclude"):
            authority.git_live_paths(gate, self.fixture.root)

    def test_file_census_hashes_content_and_all_consumed_controls(self) -> None:
        self.fixture.write("crate/src/lib.rs", "pub fn before() {}\n")
        self.fixture.write("nested/.gitignore", "ignored\n")
        before = gate.file_census_digest(self.fixture.view())
        self.fixture.write("nested/.gitignore", "different\n")
        after = gate.file_census_digest(self.fixture.view())

        self.assertNotEqual(before, after)
        self.assertIn("nested/.gitignore", gate.census_input_paths(self.fixture.view().paths))

    def test_portable_semantic_tool_identities_and_local_offline_bazel_flags(self) -> None:
        self.fixture.write(".bazelversion", "8.3.1\n")
        identities = gate.tool_identities(
            self.fixture.root, {"binary_sha256": "b" * 64}
        )
        serialized = json.dumps(identities, sort_keys=True)

        self.assertNotIn("Python ", serialized)
        self.assertNotIn("git version", serialized)
        self.assertNotIn("cargo_binary", serialized)
        self.assertIn("cargo-1.97.1", serialized)
        self.assertIn("--repository_disable_download", authority.LOCAL_BAZEL_FLAGS)
        self.assertIn("--remote_upload_local_results=false", authority.LOCAL_BAZEL_FLAGS)
        self.assertIn("--remote_accept_cached=false", authority.LOCAL_BAZEL_FLAGS)
        self.assertNotIn("readlink", (ROOT / "tools/bazel/check_rust_target_inventory.py").read_text())

    def test_orphan_rust_and_overlapping_package_roots_fail(self) -> None:
        self.fixture.write("orphan.rs")
        with self.assertRaisesRegex(gate.GateError, "orphan Rust source"):
            measured_sources(self.fixture.view())
        self.fixture.write("crate/src/lib.rs")
        nested = package("nested", "crate/nested")
        with self.assertRaisesRegex(gate.GateError, "overlapping workspace package roots"):
            gate.natural_package_owner([package(), nested], "crate/nested/src/lib.rs")

    def test_undeclared_cargo_manifest_fails_inventory_validation(self) -> None:
        for path in (
            "Cargo.toml",
            "crate/Cargo.toml",
            "crate/BUILD.bazel",
            "crate/src/lib.rs",
            "other/Cargo.toml",
        ):
            self.fixture.write(path, "")
        entry = {
            "manifest": "crate/Cargo.toml",
            "root": "crate",
            "cargo_bazel_targets": {"lib:fixture": "//crate:lib"},
            "cargo_target_roots": {"lib:fixture": "crate/src/lib.rs"},
            "bazel_production_targets": ["//crate:lib"],
            "native_unit": None,
            "focused_tests": [],
        }
        with self.assertRaisesRegex(gate.GateError, "undeclared Cargo manifests"):
            gate.validate_inventory(self.fixture.view(), [], {"packages": {"fixture": entry}})

    def test_one_hard_limit_and_exact_current_ceiling(self) -> None:
        exception = {"legacy": active_exception(21_000)}
        self.assertEqual(
            [item["code"] for item in gate.evaluate_limits([package_record("new", "new/Cargo.toml", 20_001)], {})],
            ["crate_limit"],
        )
        self.assertEqual(
            [item["code"] for item in gate.evaluate_limits([package_record("legacy", "crates/legacy/Cargo.toml", 21_001)], exception)],
            ["crate_growth"],
        )
        self.assertEqual(
            [item["code"] for item in gate.evaluate_limits([package_record("legacy", "crates/legacy/Cargo.toml", 20_999)], exception)],
            ["stale_ceiling"],
        )
        self.assertEqual(
            [item["code"] for item in gate.evaluate_limits([package_record("legacy", "crates/legacy/Cargo.toml", 20_000)], exception)],
            ["stale_exception"],
        )

    def test_bootstrap_forbids_non_admission_exception(self) -> None:
        baseline = {
            "small": {
                "package": "small",
                "manifest": "crates/small/Cargo.toml",
                "production_cloc": 19_000,
            }
        }
        policy = ledger_policy(
            active=[
                {
                    "package": "small",
                    "manifest": "crates/small/Cargo.toml",
                    "maximum_cloc": 21_000,
                }
            ],
            retired=[],
        )
        with self.assertRaisesRegex(gate.GateError, "new crate-size exceptions are forbidden"):
            gate.validate_ledger_transition(policy, None, baseline)

    def test_two_revision_ceiling_raise_is_forbidden(self) -> None:
        baseline = {
            "legacy": {
                "package": "legacy",
                "manifest": "crates/legacy/Cargo.toml",
                "production_cloc": 30_000,
            }
        }
        previous = ledger_policy(active=[active_exception(26_000)], retired=[])
        first_successor = ledger_policy(active=[active_exception(24_000)], retired=[])
        raised_successor = ledger_policy(active=[active_exception(25_000)], retired=[])

        self.assertEqual(
            gate.validate_ledger_transition(first_successor, previous, baseline), "successor"
        )
        with self.assertRaisesRegex(gate.GateError, "ceiling increase is forbidden"):
            gate.validate_ledger_transition(raised_successor, first_successor, baseline)

    def test_two_revision_retirement_is_irreversible(self) -> None:
        baseline = {
            "legacy": {
                "package": "legacy",
                "manifest": "crates/legacy/Cargo.toml",
                "production_cloc": 30_000,
            }
        }
        previous = ledger_policy(active=[active_exception(24_000)], retired=[])
        retired = ledger_policy(active=[], retired=[retired_exception()])
        resurrected = ledger_policy(active=[active_exception(21_000)], retired=[])

        self.assertEqual(gate.validate_ledger_transition(retired, previous, baseline), "successor")
        with self.assertRaisesRegex(gate.GateError, "cannot be resurrected"):
            gate.validate_ledger_transition(resurrected, retired, baseline)

    def test_full_census_digest_invalidates_on_any_component_change(self) -> None:
        components = {"files_sha256": "a" * 64, "tool_identities": {"cargo": "pinned"}}
        first = gate.census_full_digest(components)
        components["files_sha256"] = "b" * 64
        self.assertNotEqual(first, gate.census_full_digest(components))


if __name__ == "__main__":
    unittest.main()
