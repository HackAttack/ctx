#!/usr/bin/env python3
from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from check_history_capture_runtime_boundary import validate


WORKSPACE_CARGO = """\
[workspace]

[workspace.dependencies]
uuid = "1"
thiserror = "1"
serde = "1"
serde_json = "1"
sha2 = "1"
"""

RUNTIME_CARGO = """\
[dependencies]
uuid.workspace = true

[dev-dependencies]
thiserror.workspace = true
"""

JSONL_CARGO = """\
[dependencies]
ctx-history-capture-model = { path = "../ctx-history-capture-model" }
ctx-history-capture-runtime = { path = "../ctx-history-capture-runtime" }
ctx-history-core = { path = "../ctx-history-core" }
serde.workspace = true
serde_json.workspace = true
sha2.workspace = true
"""

RUNTIME_BUILD = """\
rust_library(
    name = "lib",
    deps = all_crate_deps(normal = True),
)
"""

JSONL_BUILD = """\
JSONL_DEPS = [
    "//crates/ctx-history-capture-model:lib",
    "//crates/ctx-history-capture-runtime:lib",
    "//crates/ctx-history-core:lib",
]

rust_library(
    name = "lib",
    deps = all_crate_deps(normal = True) + JSONL_DEPS,
)
"""


class BoundaryMutationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        root = Path(self.temporary.name)
        self.workspace_manifest = root / "Cargo.toml"
        self.runtime_manifest = root / "runtime-Cargo.toml"
        self.runtime_build = root / "runtime-BUILD.bazel"
        self.jsonl_manifest = root / "jsonl-Cargo.toml"
        self.jsonl_build = root / "jsonl-BUILD.bazel"
        self.workspace_manifest.write_text(WORKSPACE_CARGO, encoding="utf-8")
        self.runtime_manifest.write_text(RUNTIME_CARGO, encoding="utf-8")
        self.runtime_build.write_text(RUNTIME_BUILD, encoding="utf-8")
        self.jsonl_manifest.write_text(JSONL_CARGO, encoding="utf-8")
        self.jsonl_build.write_text(JSONL_BUILD, encoding="utf-8")

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def validate(self) -> None:
        validate(
            self.workspace_manifest,
            self.runtime_manifest,
            self.runtime_build,
            self.jsonl_manifest,
            self.jsonl_build,
        )

    def test_minimal_runtime_boundary_passes(self) -> None:
        self.validate()

    def test_package_rename_is_rejected(self) -> None:
        self.jsonl_manifest.write_text(
            JSONL_CARGO
            + '\n[dev-dependencies]\nindex_alias = { package = "ctx-history-index", path = "../ctx-history-index" }\n',
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "forbidden Cargo dependencies"):
            self.validate()

    def test_workspace_inherited_package_rename_is_rejected(self) -> None:
        self.workspace_manifest.write_text(
            WORKSPACE_CARGO
            + '\nindex_alias = { package = "ctx-history-index", version = "1" }\n',
            encoding="utf-8",
        )
        self.jsonl_manifest.write_text(
            JSONL_CARGO + "\n[dev-dependencies]\nindex_alias.workspace = true\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "forbidden Cargo dependencies"):
            self.validate()

    def test_normal_dev_and_build_dependency_variants_are_rejected(self) -> None:
        for table in ("dependencies", "dev-dependencies", "build-dependencies"):
            with self.subTest(table=table):
                addition = (
                    'ctx-history-index = { path = "../ctx-history-index" }\n'
                    if table == "dependencies"
                    else f'\n[{table}]\nctx-history-index = {{ path = "../ctx-history-index" }}\n'
                )
                self.jsonl_manifest.write_text(
                    JSONL_CARGO + addition,
                    encoding="utf-8",
                )
                with self.assertRaisesRegex(ValueError, "forbidden Cargo dependencies"):
                    self.validate()
                self.jsonl_manifest.write_text(JSONL_CARGO, encoding="utf-8")

    def test_target_specific_normal_dev_and_build_variants_are_rejected(self) -> None:
        for table in ("dependencies", "dev-dependencies", "build-dependencies"):
            with self.subTest(table=table):
                self.jsonl_manifest.write_text(
                    JSONL_CARGO
                    + f"\n[target.'cfg(unix)'.{table}]\n"
                    + 'index_alias = { package = "ctx-history-index-format", path = "../ctx-history-index-format" }\n',
                    encoding="utf-8",
                )
                with self.assertRaisesRegex(ValueError, "forbidden Cargo dependencies"):
                    self.validate()
                self.jsonl_manifest.write_text(JSONL_CARGO, encoding="utf-8")

    def test_ambiguous_workspace_dependency_is_rejected(self) -> None:
        self.jsonl_manifest.write_text(
            JSONL_CARGO
            + '\n[dev-dependencies]\nindex_alias = { package = "ctx-history-index", workspace = true }\n',
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "cannot combine workspace inheritance"):
            self.validate()

    def test_malformed_dependency_is_rejected(self) -> None:
        self.jsonl_manifest.write_text(
            JSONL_CARGO + "\n[dev-dependencies]\nindex_alias = 1\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "must be a string or inline table"):
            self.validate()

    def test_runtime_build_dependency_outside_allowlist_is_rejected(self) -> None:
        self.runtime_manifest.write_text(
            RUNTIME_CARGO + '\n[build-dependencies]\ncc = "1"\n',
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "Cargo build dependencies drifted"):
            self.validate()

    def test_runtime_target_dependency_evasion_is_rejected(self) -> None:
        self.runtime_manifest.write_text(
            RUNTIME_CARGO
            + "\n[target.'cfg(unix)'.dependencies]\n"
            + 'ctx-history-index = { path = "../ctx-history-index" }\n',
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "forbidden Cargo dependencies"):
            self.validate()

    def test_composed_bazel_label_is_rejected(self) -> None:
        self.jsonl_build.write_text(
            JSONL_BUILD.replace(
                "all_crate_deps(normal = True) + JSONL_DEPS",
                'all_crate_deps(normal = True) + ["//crates/ctx-history-index:lib"]',
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "Bazel deps must be exactly"):
            self.validate()

    def test_loaded_bazel_label_is_rejected(self) -> None:
        self.jsonl_build.write_text(
            'load("//tools:deps.bzl", "JSONL_DEPS")\n'
            'rust_library(\n'
            '    name = "lib",\n'
            '    deps = all_crate_deps(normal = True) + JSONL_DEPS,\n'
            ')\n',
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "literal JSONL_DEPS"):
            self.validate()

    def test_bazel_comments_do_not_create_false_positive(self) -> None:
        self.jsonl_build.write_text(
            JSONL_BUILD
            + '# "//crates/ctx-history-index:lib" must remain forbidden\n',
            encoding="utf-8",
        )
        self.validate()

    def test_jsonl_index_format_direct_label_is_rejected(self) -> None:
        self.jsonl_build.write_text(
            JSONL_BUILD.replace(
                '"//crates/ctx-history-core:lib",',
                '"//crates/ctx-history-index-format:lib",',
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "direct dependency inventory drifted"):
            self.validate()

    def test_cli_returns_nonzero_for_invalid_input(self) -> None:
        self.jsonl_manifest.write_text("[dependencies\n", encoding="utf-8")
        completed = subprocess.run(
            [
                sys.executable,
                str(Path(__file__).with_name("check_history_capture_runtime_boundary.py")),
                str(self.workspace_manifest),
                str(self.runtime_manifest),
                str(self.runtime_build),
                str(self.jsonl_manifest),
                str(self.jsonl_build),
            ],
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(completed.returncode, 0)
        self.assertIn("Expected ']'", completed.stderr)


if __name__ == "__main__":
    unittest.main()
