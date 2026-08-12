#!/usr/bin/env python3
from __future__ import annotations

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
INVENTORY = ROOT / "scripts" / "release" / "cargo-release-inventory.py"
BUILD_INFO = ROOT / "scripts" / "release" / "linux-factory-build-info.py"
RELEASE_SBOM = ROOT / "scripts" / "release-sbom.py"
SCHEMA = ROOT / "contracts" / "release-candidate-manifest-v1.schema.json"


def load(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


inventory = load(INVENTORY, "cargo_release_inventory")
build_info = load(BUILD_INFO, "linux_factory_build_info")
release_sbom = load(RELEASE_SBOM, "release_sbom")


class LinuxReleaseFactoryTest(unittest.TestCase):
    def test_selected_graph_uses_only_reachable_packages(self) -> None:
        metadata = {
            "packages": [
                {"id": "ctx", "name": "ctx", "source": None},
                {"id": "reachable", "name": "reachable", "source": "registry"},
                {"id": "foreign", "name": "foreign", "source": "registry"},
            ],
            "resolve": {
                "nodes": [
                    {"id": "ctx", "deps": [{"pkg": "reachable"}]},
                    {"id": "reachable", "deps": []},
                    {"id": "foreign", "deps": []},
                ]
            },
        }
        self.assertEqual(inventory.selected_package_ids(metadata), {"ctx", "reachable"})

    def test_material_inventory_is_portable_and_complete(self) -> None:
        with tempfile.TemporaryDirectory() as source_directory, tempfile.TemporaryDirectory() as directory:
            source = Path(source_directory) / "Cargo.toml"
            source.write_text("[package]\nname='fixture'\nversion='1.0.0'\n")
            records = [{"kind": "main", "logical": "crates/fixture/Cargo.toml", "path": str(source)}]
            portable = inventory.stage_materials(records, Path(directory))
            self.assertNotIn(str(ROOT), json.dumps(portable))
            self.assertEqual(portable, [{"kind": "main", "logical": "crates/fixture/Cargo.toml"}])
            self.assertTrue((Path(directory) / "crates/fixture/Cargo.toml").is_file())

    def test_build_info_requires_sdk_exactly_for_macos(self) -> None:
        matrix = ROOT / "contracts" / "release-targets-v1.json"
        self.assertEqual(build_info.target(matrix, "linux-x64")["os"], "linux")
        self.assertEqual(build_info.target(matrix, "macos-arm64")["os"], "macos")
        with self.assertRaisesRegex(ValueError, "exact platform"):
            build_info.target(matrix, "freebsd-x64")

    def test_factory_candidate_uses_real_schema_construction_branch(self) -> None:
        schema = json.loads(SCHEMA.read_text(encoding="utf-8"))
        construction = schema["properties"]["construction"]
        factory_branch = construction["allOf"][1]
        self.assertEqual(
            factory_branch["if"]["properties"]["authority"]["const"],
            "linux-cross-cargo-zigbuild-v1",
        )
        matrix = json.dumps(
            {
                "schema_version": 1,
                "targets": [
                    {
                        "id": "linux-x64",
                        "public_rust_target": "x86_64-unknown-linux-gnu",
                        "public_construction_authority": "linux-cross-cargo-zigbuild-v1",
                        "public_construction_label": "scripts/release/build-public-candidate-on-linux.sh",
                    }
                ],
            },
            separators=(",", ":"),
        ).encode()
        target = release_sbom.target_contract(
            matrix, "linux-x64", "linux-x64", "x86_64-unknown-linux-gnu"
        )
        candidate_construction = {
            "authority": target["public_construction_authority"],
            "label": target["public_construction_label"],
        }
        self.assertEqual(
            candidate_construction["label"],
            factory_branch["then"]["properties"]["label"]["const"],
        )

    def test_factory_script_pins_all_external_tools(self) -> None:
        source = (ROOT / "scripts" / "release" / "build-public-candidate-on-linux.sh").read_text()
        for value in ("1.97.1", "0.15.2", "0.23.0", "0.29.0"):
            self.assertIn(value, source)
        self.assertIn("--diagnostic-unsigned", source)
        self.assertIn("official release requires", source)
        self.assertIn("llvm-strip -S -x", source)
        self.assertIn("/usr/bin/llvm-readobj", source)
        self.assertIn("ctx-release-factory.json", source)
        self.assertIn('"${cargo_zigbuild_bin}" zigbuild', source)
        self.assertNotIn("cargo zigbuild", source)

    def test_cargo_zigbuild_resolution_rejects_shadow_and_returns_absolute_path(self) -> None:
        helper = ROOT / "scripts" / "release" / "resolve-cargo-zigbuild.py"
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            trusted = root / "trusted"
            shadow = root / "shadow"
            trusted.mkdir()
            shadow.mkdir()
            trusted_tool = trusted / "cargo-zigbuild"
            shadow_tool = shadow / "cargo-zigbuild"
            trusted_tool.write_text(
                "#!/bin/sh\n"
                "if [ \"$1\" = --version ]; then printf '%s\\n' 'cargo-zigbuild 0.23.0'; else printf trusted; fi\n",
                encoding="utf-8",
            )
            shadow_tool.write_text(
                "#!/bin/sh\nprintf '%s\\n' 'cargo-zigbuild 0.22.0'\n",
                encoding="utf-8",
            )
            trusted_tool.chmod(stat.S_IRWXU)
            shadow_tool.chmod(stat.S_IRWXU)
            rejected = subprocess.run(
                [sys.executable, os.fspath(helper), "--expected-version", "0.23.0"],
                env={"PATH": os.fspath(shadow) + os.pathsep + os.fspath(trusted)},
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(rejected.returncode, 0)
            selected = subprocess.run(
                [sys.executable, os.fspath(helper), "--expected-version", "0.23.0"],
                env={"PATH": os.fspath(trusted) + os.pathsep + os.fspath(shadow)},
                check=True,
                capture_output=True,
                text=True,
            )
            resolved = json.loads(selected.stdout)
            self.assertEqual(resolved["path"], os.path.realpath(trusted_tool))
            self.assertEqual(resolved["version"], "0.23.0")
            self.assertEqual(
                subprocess.check_output(
                    [resolved["path"], "zigbuild"],
                    env={"PATH": os.fspath(shadow) + os.pathsep + os.environ.get("PATH", "")},
                    text=True,
                ),
                "trusted",
            )

    def test_factory_build_info_does_not_claim_legacy_bazel_inputs(self) -> None:
        source = (ROOT / "scripts" / "release" / "linux-factory-build-info.py").read_text()
        self.assertIn('"linux_build": None', source)
        self.assertIn('"release_factory": {', source)


if __name__ == "__main__":
    unittest.main()
