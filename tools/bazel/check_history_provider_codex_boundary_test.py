#!/usr/bin/env python3
import tempfile
import unittest
from pathlib import Path

from check_history_provider_codex_boundary import BoundaryError, validate


class CodexBoundaryMutationTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name)
        self.manifest = root / "Cargo.toml"
        self.build = root / "BUILD.bazel"
        self.lib = root / "lib.rs"
        self.registration = root / "codex.rs"
        self.manifest.write_text('[package]\nname = "ctx-history-provider-codex"\n[dependencies]\n' + ''.join(f'{x} = {{ workspace = true }}\n' if x in {"base64", "chrono", "serde", "serde_json", "sha2", "tempfile", "thiserror", "uuid", "zstd"} else f'{x} = {{ path = "../{x}" }}\n' for x in sorted({"base64", "chrono", "ctx-history-capture-model", "ctx-history-capture-runtime", "ctx-history-core", "ctx-history-provider-runtime", "ctx-history-source-io", "serde", "serde_json", "sha2", "tempfile", "thiserror", "uuid", "zstd"})), encoding="utf-8")
        self.build.write_text('"//crates/ctx-history-provider-runtime:lib",\n', encoding="utf-8")
        self.lib.write_text('pub struct Pack;\n', encoding="utf-8")
        self.registration.write_text('CaptureProviderRuntime CodexPromptHistoryJsonlFamilyAdapterV0::<CaptureProviderRuntime>', encoding="utf-8")

    def tearDown(self):
        self.tmp.cleanup()

    def test_passes(self):
        validate(self.manifest, self.build, self.lib, self.registration)

    def test_capture_dependency_rejected(self):
        self.build.write_text('//crates/ctx-history-capture:lib', encoding="utf-8")
        with self.assertRaisesRegex(BoundaryError, "capture/index"):
            validate(self.manifest, self.build, self.lib, self.registration)

    def test_forbidden_source_name_rejected(self):
        self.lib.write_text('use ctx_history_index::IndexError;', encoding="utf-8")
        with self.assertRaisesRegex(BoundaryError, "forbidden"):
            validate(self.manifest, self.build, self.lib, self.registration)


if __name__ == "__main__":
    unittest.main()
