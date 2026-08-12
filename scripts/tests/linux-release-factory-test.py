#!/usr/bin/env python3
from __future__ import annotations

import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
INVENTORY = ROOT / "scripts" / "release" / "cargo-release-inventory.py"
BUILD_INFO = ROOT / "scripts" / "release" / "linux-factory-build-info.py"


def load(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


inventory = load(INVENTORY, "cargo_release_inventory")
build_info = load(BUILD_INFO, "linux_factory_build_info")


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

    def test_factory_script_pins_all_external_tools(self) -> None:
        source = (ROOT / "scripts" / "release" / "build-public-candidate-on-linux.sh").read_text()
        for value in ("1.97.1", "0.15.2", "0.23.0", "0.29.0"):
            self.assertIn(value, source)
        self.assertIn("--diagnostic-unsigned", source)
        self.assertIn("official release requires", source)
        self.assertIn("llvm-strip -S -x", source)
        self.assertIn("/usr/bin/llvm-readobj", source)
        self.assertIn("ctx-release-factory.json", source)

    def test_factory_build_info_does_not_claim_legacy_bazel_inputs(self) -> None:
        source = (ROOT / "scripts" / "release" / "linux-factory-build-info.py").read_text()
        self.assertIn('"linux_build": None', source)
        self.assertIn('"release_factory": {', source)


if __name__ == "__main__":
    unittest.main()
