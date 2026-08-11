#!/usr/bin/env python3
from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from check_history_capture_runtime_boundary import validate


class BoundaryMutationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        root = Path(self.temporary.name)
        self.manifest = root / "Cargo.toml"
        self.build = root / "BUILD.bazel"
        self.manifest.write_text(
            "[dependencies]\nuuid.workspace = true\n\n[dev-dependencies]\nthiserror.workspace = true\n",
            encoding="utf-8",
        )
        self.build.write_text("rust_library(name = \"lib\")\n", encoding="utf-8")

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def test_minimal_runtime_boundary_passes(self) -> None:
        validate(self.manifest, self.build)

    def test_index_dependency_is_rejected(self) -> None:
        self.manifest.write_text(
            self.manifest.read_text(encoding="utf-8")
            .replace(
                "\n[dev-dependencies]",
                '\nctx-history-index = { path = "../ctx-history-index" }\n[dev-dependencies]',
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "forbidden Cargo dependencies"):
            validate(self.manifest, self.build)

    def test_provider_label_is_rejected(self) -> None:
        self.build.write_text(
            'deps = ["//crates/ctx-history-capture:lib"]\n', encoding="utf-8"
        )
        with self.assertRaisesRegex(ValueError, "forbidden Bazel"):
            validate(self.manifest, self.build)


if __name__ == "__main__":
    unittest.main()
