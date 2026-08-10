#!/usr/bin/env python3

from __future__ import annotations

import hashlib
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest


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
    excluded: set[str] | None = None,
    packages: list[dict[str, str]] | None = None,
) -> dict[str, set[str]]:
    package_list = packages or [package()]
    inventory = {
        item["package"]: {
            "targets": {},
            "excluded_test_sources": excluded or set(),
        }
        for item in package_list
    }
    return gate.production_sources(view, package_list, inventory)


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


def rust_rule(kind: str, label: str, sources: list[str], *, testonly: bool = False) -> str:
    testonly_xml = "<boolean name='testonly' value='true'/>" if testonly else ""
    labels = "".join(f"<label value='{item}'/>" for item in sources)
    return (
        f"<rule class='{kind}' name='{label}'>{testonly_xml}"
        f"<list name='srcs'>{labels}</list><label name='crate_root' value='{sources[0]}'/></rule>"
    )


def package_record(name: str, manifest: str, code: int) -> dict[str, object]:
    return {"package": name, "manifest": manifest, "production_cloc": code}


class RustCrateSizeAuthorityTest(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = Fixture()
        self.addCleanup(self.fixture.close)

    def test_package_union_counts_cfg_feature_path_dead_inline_and_build_sources(self) -> None:
        self.fixture.write(
            "crate/src/lib.rs",
            "#[cfg(test)] mod inline { fn test() {} }\n"
            "#[cfg(feature = \"optional-dep\")] mod optional;\n"
            "#[cfg_attr(feature = \"alternate\", path = \"alternate.rs\")] mod selected;\n",
        )
        for path in (
            "crate/src/optional.rs",
            "crate/src/alternate.rs",
            "crate/src/selected.rs",
            "crate/src/test_support.rs",
            "crate/src/dead.rs",
            "crate/build.rs",
            "crate/build_helper.rs",
            "crate/tests/support.rs",
            "snapshots/crate/src/old.rs",
        ):
            self.fixture.write(path)
        self.fixture.write("crate/tests/integration.rs")

        sources = measured_sources(
            self.fixture.view(),
            excluded={"crate/tests/integration.rs"},
        )["fixture"]

        self.assertNotIn("crate/tests/integration.rs", sources)
        self.assertIn("crate/tests/support.rs", sources)
        self.assertIn("crate/src/optional.rs", sources)
        self.assertIn("crate/src/alternate.rs", sources)
        self.assertIn("crate/src/selected.rs", sources)
        self.assertIn("crate/src/test_support.rs", sources)
        self.assertIn("crate/src/dead.rs", sources)
        self.assertIn("crate/build.rs", sources)
        self.assertIn("snapshots/crate/src/old.rs", sources)

    def test_exclusion_requires_exclusive_cargo_or_rust_test_ownership(self) -> None:
        pkg = package()
        targets = {
            "lib:fixture": gate.CargoTarget("lib:fixture", "lib", "fixture", "crate/src/lib.rs"),
            "test:integration": gate.CargoTarget(
                "test:integration", "test", "integration", "crate/tests/integration.rs"
            ),
        }

        excluded = authority.exclusive_test_sources(
            gate,
            pkg,
            targets,
            {"crate/src/lib.rs", "crate/src/shared.rs"},
            {"crate/src/shared.rs", "crate/src/unit_only.rs", "crate/tests/support.rs"},
        )

        self.assertEqual(
            excluded,
            ["crate/src/unit_only.rs", "crate/tests/integration.rs", "crate/tests/support.rs"],
        )

    def test_unknown_cfg_attr_path_counts_default_and_alternate_files(self) -> None:
        self.fixture.write(
            "crate/src/lib.rs",
            "#[cfg_attr(feature = \"alternate\", path = \"alternate.rs\")] mod selected;\n",
        )
        self.fixture.write("crate/src/selected.rs")
        self.fixture.write("crate/src/alternate.rs")

        sources = measured_sources(self.fixture.view())["fixture"]

        self.assertIn("crate/src/selected.rs", sources)
        self.assertIn("crate/src/alternate.rs", sources)

    def test_testonly_library_is_production_and_overrides_rust_test_claim(self) -> None:
        raw = xml_query(
            source("//crate:src/shared.rs"),
            rust_rule("rust_library", "//crate:test_support", ["//crate:src/shared.rs"], testonly=True),
            rust_rule("rust_test", "//crate:unit_tests", ["//crate:src/shared.rs"]),
        )
        records, _ = authority.parse_bazel_query_xml(gate, raw)
        production, production_sources, test_sources = authority.validate_graph_and_labels(
            gate,
            [package()],
            records,
            {
                "//crate:test_support": "rust_library rule",
                "//crate:unit_tests": "rust_test rule",
            },
            {"crate/src/shared.rs"},
        )

        self.assertEqual(production["fixture"], ["//crate:test_support"])
        self.assertEqual(production_sources["fixture"], {"crate/src/shared.rs"})
        self.assertEqual(test_sources["fixture"], {"crate/src/shared.rs"})
        self.assertEqual(
            authority.exclusive_test_sources(
                gate, package(), {}, production_sources["fixture"], test_sources["fixture"]
            ),
            [],
        )

    def test_bazel_filegroup_overapproximates_all_select_sources(self) -> None:
        raw = xml_query(
            source("//crate:src/default.rs"),
            source("//crate:src/windows.rs"),
            "<rule class='filegroup' name='//crate:selected'><list name='srcs'>"
            "<label value='//crate:src/default.rs'/><label value='//crate:src/windows.rs'/>"
            "</list></rule>",
            rust_rule("rust_library", "//crate:lib", ["//crate:selected"]),
        )

        records, canonical = authority.parse_bazel_query_xml(gate, raw)

        self.assertEqual(
            records,
            [
                {
                    "label": "//crate:lib",
                    "kind": "rust_library",
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
            {"label": "//outside:lib", "kind": "rust_library", "sources": ["crate/src/bazel_only.rs"]}
        ]
        with self.assertRaisesRegex(gate.GateError, "outside every Cargo workspace package"):
            authority.validate_graph_and_labels(
                gate,
                [package()],
                records,
                {"//outside:lib": "rust_library rule"},
                self.fixture.view().paths,
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

    def test_symlinked_rust_source_is_not_a_hermetic_checked_input(self) -> None:
        self.fixture.write("outside.rs")
        source_path = self.fixture.root / "crate/src/lib.rs"
        source_path.parent.mkdir(parents=True)
        source_path.symlink_to(self.fixture.root / "outside.rs")

        with self.assertRaisesRegex(gate.GateError, "declared source is unavailable"):
            gate.file_census_digest(self.fixture.view())
        declared_view = gate.SourceView(self.fixture.root, self.fixture.view().paths, allow_symlinks=True)
        self.assertIsInstance(gate.file_census_digest(declared_view), str)

    def test_cargo_metadata_is_path_canonical_and_features_cannot_hide_sources(self) -> None:
        self.fixture.write("crate/Cargo.toml", "[package]\nname='fixture'\nversion='0.1.0'\n")
        self.fixture.write("crate/src/lib.rs")
        self.fixture.write("crate/src/implicit_optional.rs")
        self.fixture.write("crate/tests/integration.rs")
        first, first_bytes = gate.canonical_cargo_metadata(
            metadata(self.fixture.root, features={"implicit-optional": ["dep:implicit-optional"]}),
            self.fixture.root,
        )
        second, second_bytes = gate.canonical_cargo_metadata(
            metadata(self.fixture.root, features={}),
            self.fixture.root,
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

    def test_file_census_hashes_path_blob_content_and_authority_packages(self) -> None:
        self.fixture.write("crate/src/lib.rs", "pub fn before() {}\n")
        before = gate.file_census_digest(self.fixture.view())
        self.fixture.write("crate/src/lib.rs", "pub fn after() {}\n")
        after = gate.file_census_digest(self.fixture.view())
        packages_a = {"fixture": {"excluded_test_sources": []}}
        packages_b = {"fixture": {"excluded_test_sources": ["crate/src/lib.rs"]}}

        self.assertNotEqual(before, after)
        self.assertNotEqual(
            hashlib.sha256(gate.canonical_bytes(packages_a)).digest(),
            hashlib.sha256(gate.canonical_bytes(packages_b)).digest(),
        )

    def test_orphan_rust_and_overlapping_package_roots_fail(self) -> None:
        self.fixture.write("orphan.rs")
        with self.assertRaisesRegex(gate.GateError, "orphan Rust source"):
            measured_sources(self.fixture.view())
        self.fixture.write("crate/src/lib.rs")
        nested = package("nested", "crate/nested")
        with self.assertRaisesRegex(gate.GateError, "overlapping workspace package roots"):
            gate.natural_package_owner([package(), nested], "crate/nested/src/lib.rs")

    def test_undeclared_cargo_manifest_fails_inventory_validation(self) -> None:
        for path in ("Cargo.toml", "crate/Cargo.toml", "crate/BUILD.bazel", "crate/src/lib.rs", "other/Cargo.toml"):
            self.fixture.write(path, "")
        entry = {
            "manifest": "crate/Cargo.toml",
            "root": "crate",
            "targets": {"lib:fixture": "//crate:lib"},
            "cargo_target_roots": {"lib:fixture": "crate/src/lib.rs"},
            "bazel_production_targets": ["//crate:lib"],
            "excluded_test_sources": [],
            "native_unit": "//crate:unit_tests",
            "focused_tests": [],
        }
        with self.assertRaisesRegex(gate.GateError, "undeclared Cargo manifests"):
            gate.validate_inventory(self.fixture.view(), [], {"packages": {"fixture": entry}})

    def test_one_hard_limit_and_shrink_only_ledger(self) -> None:
        exception = {
            "legacy": {
                "package": "legacy",
                "manifest": "crates/legacy/Cargo.toml",
                "maximum_cloc": 21_000,
            }
        }
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

    def test_new_exception_is_forbidden_by_immutable_baseline(self) -> None:
        policy = {
            "temporary_exceptions": [
                {"package": "small", "manifest": "crates/small/Cargo.toml", "maximum_cloc": 21_000}
            ]
        }
        baseline = {
            "small": {"package": "small", "manifest": "crates/small/Cargo.toml", "production_cloc": 19_000}
        }
        with self.assertRaisesRegex(gate.GateError, "new crate-size exceptions are forbidden"):
            gate.validate_exceptions(policy, baseline)


if __name__ == "__main__":
    unittest.main()
