#!/usr/bin/env python3

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from check_history_provider_sqlite_logical_boundary import (
    BoundaryError,
    EXPECTED_DEPENDENCIES,
    EXPECTED_DEV_DEPENDENCIES,
    EXPECTED_INTERNAL_BAZEL,
    EXPECTED_TRANSITIVE_BAZEL,
    validate_bazel_inventory,
    validate_manifest,
    validate_neutral_source,
)


def toml_value(value: object) -> str:
    if value is True:
        return "true"
    if isinstance(value, str):
        return f'"{value}"'
    if isinstance(value, list):
        return "[" + ", ".join(toml_value(item) for item in value) + "]"
    if isinstance(value, dict):
        return "{ " + ", ".join(f"{key} = {toml_value(item)}" for key, item in value.items()) + " }"
    raise AssertionError(value)


def manifest_text() -> str:
    lines = [
        "[package]",
        'name = "ctx-history-providers-sqlite-logical"',
        'version = "0.0.0"',
        "",
        "[dependencies]",
    ]
    lines.extend(f"{name} = {toml_value(value)}" for name, value in EXPECTED_DEPENDENCIES.items())
    lines.extend(["", "[dev-dependencies]"])
    lines.extend(f"{name} = {toml_value(value)}" for name, value in EXPECTED_DEV_DEPENDENCIES.items())
    return "\n".join(lines) + "\n"


class BoundaryMutationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.manifest = self.root / "Cargo.toml"
        self.manifest.write_text(manifest_text(), encoding="utf-8")

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def test_exact_cargo_and_bazel_inventories_pass(self) -> None:
        validate_manifest(self.manifest)
        validate_bazel_inventory(EXPECTED_INTERNAL_BAZEL, "direct")

    def test_capture_or_index_dependency_is_rejected(self) -> None:
        for dependency in ("ctx-history-capture", "ctx-history-index"):
            with self.subTest(dependency=dependency):
                self.manifest.write_text(
                    manifest_text().replace(
                        "[dependencies]\n",
                        f'[dependencies]\n{dependency} = {{ path = "../{dependency}" }}\n',
                    ),
                    encoding="utf-8",
                )
                with self.assertRaises(BoundaryError):
                    validate_manifest(self.manifest)

    def test_target_specific_dependency_bypass_is_rejected(self) -> None:
        self.manifest.write_text(
            manifest_text()
            + '\n[target.\'cfg(unix)\'.dependencies]\nctx-history-index = { path = "../ctx-history-index" }\n',
            encoding="utf-8",
        )
        with self.assertRaises(BoundaryError):
            validate_manifest(self.manifest)

    def test_bazel_composition_edge_is_rejected(self) -> None:
        with self.assertRaises(BoundaryError):
            validate_bazel_inventory(
                EXPECTED_INTERNAL_BAZEL | {"//crates/ctx-history-capture-composition:lib"},
                "transitive",
            )

    def test_transitive_inventory_has_no_jsonl_exception(self) -> None:
        validate_bazel_inventory(EXPECTED_TRANSITIVE_BAZEL, "transitive")

    def test_runtime_or_discovery_pack_assembly_is_rejected(self) -> None:
        source = self.root / "src"
        source.mkdir()
        (source / "lib.rs").write_text(
            "use ctx_history_providers_sqlite_logical::registration;\n",
            encoding="utf-8",
        )
        with self.assertRaises(BoundaryError):
            validate_neutral_source(source, "neutral fixture")


if __name__ == "__main__":
    unittest.main()
