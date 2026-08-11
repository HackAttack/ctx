#!/usr/bin/env python3
from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from check_history_capture_runtime_boundary import validate


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


class BoundaryMutationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        root = Path(self.temporary.name)
        self.runtime_manifest = root / "runtime-Cargo.toml"
        self.runtime_build = root / "runtime-BUILD.bazel"
        self.jsonl_manifest = root / "jsonl-Cargo.toml"
        self.jsonl_build = root / "jsonl-BUILD.bazel"
        self.runtime_manifest.write_text(RUNTIME_CARGO, encoding="utf-8")
        self.runtime_build.write_text("rust_library(name = \"lib\")\n", encoding="utf-8")
        self.jsonl_manifest.write_text(JSONL_CARGO, encoding="utf-8")
        self.jsonl_build.write_text("rust_library(name = \"lib\")\n", encoding="utf-8")

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def test_minimal_runtime_boundary_passes(self) -> None:
        validate(
            self.runtime_manifest,
            self.runtime_build,
            self.jsonl_manifest,
            self.jsonl_build,
        )

    def test_index_dependency_is_rejected(self) -> None:
        self.runtime_manifest.write_text(
            self.runtime_manifest.read_text(encoding="utf-8")
            .replace(
                "\n[dev-dependencies]",
                '\nctx-history-index = { path = "../ctx-history-index" }\n[dev-dependencies]',
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "forbidden Cargo dependencies"):
            validate(
                self.runtime_manifest,
                self.runtime_build,
                self.jsonl_manifest,
                self.jsonl_build,
            )

    def test_jsonl_index_dependency_is_rejected(self) -> None:
        self.jsonl_manifest.write_text(
            self.jsonl_manifest.read_text(encoding="utf-8")
            + '\n[build-dependencies]\nctx-history-index = { path = "../ctx-history-index" }\n',
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "ctx-history-jsonl.*forbidden Cargo"):
            validate(
                self.runtime_manifest,
                self.runtime_build,
                self.jsonl_manifest,
                self.jsonl_build,
            )

    def test_jsonl_index_format_bazel_dependency_is_rejected(self) -> None:
        self.jsonl_build.write_text(
            'deps = ["@crates//:ctx-history-index-format"]\n', encoding="utf-8"
        )
        with self.assertRaisesRegex(ValueError, "ctx-history-jsonl.*forbidden Bazel"):
            validate(
                self.runtime_manifest,
                self.runtime_build,
                self.jsonl_manifest,
                self.jsonl_build,
            )

    def test_target_specific_dependency_evasion_is_rejected(self) -> None:
        self.jsonl_manifest.write_text(
            self.jsonl_manifest.read_text(encoding="utf-8")
            + "\n[target.'cfg(unix)'.dependencies]\n"
            + 'ctx-history-index-format = { path = "../ctx-history-index-format" }\n',
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "ctx-history-jsonl.*forbidden Cargo"):
            validate(
                self.runtime_manifest,
                self.runtime_build,
                self.jsonl_manifest,
                self.jsonl_build,
            )

    def test_runtime_build_dependency_outside_allowlist_is_rejected(self) -> None:
        self.runtime_manifest.write_text(
            self.runtime_manifest.read_text(encoding="utf-8")
            + '\n[build-dependencies]\ncc = "1"\n',
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "Cargo build dependencies drifted"):
            validate(
                self.runtime_manifest,
                self.runtime_build,
                self.jsonl_manifest,
                self.jsonl_build,
            )

    def test_provider_label_is_rejected(self) -> None:
        self.runtime_build.write_text(
            'deps = ["//crates/ctx-history-capture:lib"]\n', encoding="utf-8"
        )
        with self.assertRaisesRegex(ValueError, "forbidden Bazel"):
            validate(
                self.runtime_manifest,
                self.runtime_build,
                self.jsonl_manifest,
                self.jsonl_build,
            )


if __name__ == "__main__":
    unittest.main()
