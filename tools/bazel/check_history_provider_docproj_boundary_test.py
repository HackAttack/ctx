#!/usr/bin/env python3
"""Adversarial mutations for the document-projection provider-pack boundary."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from check_history_provider_docproj_boundary import BoundaryError, EXPECTED_SOURCES, validate


MANIFEST = """\
[package]
name = "ctx-history-provider-docproj"

[dependencies]
chrono.workspace = true
ctx-history-capture-model = { path = "../ctx-history-capture-model" }
ctx-history-capture-runtime = { path = "../ctx-history-capture-runtime" }
ctx-history-core = { path = "../ctx-history-core" }
ctx-history-provider-runtime = { path = "../ctx-history-provider-runtime" }
ctx-history-source-discovery = { path = "../ctx-history-source-discovery" }
ctx-history-source-io = { path = "../ctx-history-source-io" }
ctx-history-source-sqlite = { path = "../ctx-history-source-sqlite" }
rusqlite.workspace = true
serde.workspace = true
serde_json.workspace = true
sha2.workspace = true
thiserror.workspace = true

[dev-dependencies]
ctx-history-source-io = { path = "../ctx-history-source-io", features = ["test-support"] }
ctx-history-source-sqlite = { path = "../ctx-history-source-sqlite", features = ["test-support"] }
tempfile.workspace = true
"""
BUILD = "\n".join(f'"//crates/{label}",' for label in (
    "ctx-history-capture-model:lib", "ctx-history-capture-runtime:lib",
    "ctx-history-core:lib", "ctx-history-provider-runtime:lib",
    "ctx-history-provider-runtime:test_support_lib", "ctx-history-source-discovery:lib",
    "ctx-history-source-io:lib", "ctx-history-source-io:test_support_lib",
    "ctx-history-source-sqlite:lib", "ctx-history-source-sqlite:test_support_lib",
))
SOURCE = """\
decode_document_full_snapshot_checkpoint DocumentFullSnapshotCheckpointError
DocumentLeafExecutionPolicy::Serial DocumentLeafExecutionPolicy::Independent
AUGGIE_SESSION_JSON_SOURCE_FORMAT NANOCLAW_SOURCE_FORMAT OPENHANDS_FILE_EVENTS_SOURCE_FORMAT
ProviderRuntimeBinding ReplacementDocumentTree ProviderChangedDocumentSink
"""
CAPTURE_MANIFEST = '[dependencies]\nctx-history-provider-docproj = { path = "../ctx-history-provider-docproj" }\n'
CAPTURE_BUILD = '"//crates/ctx-history-provider-docproj:lib"\n'
FACADES = "pub(crate) mod nanoclaw;\npub(crate) mod openhands;\n"
DOCUMENT = 'NanoClawDocumentTreeAdapter::<CaptureProviderRuntime>::new_with_base_sources'
EVENT_FILE = 'OpenHandsEventFileAdapterV2::<CaptureProviderRuntime>'
EXPECTED_ADVERSARIAL_MUTATION_COUNT = 8


class DocumentProjectionBoundaryMutationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        root = Path(self.temporary.name)
        self.manifest = root / "pack/Cargo.toml"
        self.build = root / "pack/BUILD.bazel"
        self.capture_manifest = root / "capture/Cargo.toml"
        self.capture_build = root / "capture/BUILD.bazel"
        self.facades = root / "capture/facades.rs"
        self.document = root / "capture/document.rs"
        self.event_file = root / "capture/event_file.rs"
        for path, content in ((self.manifest, MANIFEST), (self.build, BUILD),
                              (self.capture_manifest, CAPTURE_MANIFEST), (self.capture_build, CAPTURE_BUILD),
                              (self.facades, FACADES), (self.document, DOCUMENT), (self.event_file, EVENT_FILE)):
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content, encoding="utf-8")
        for relative in EXPECTED_SOURCES:
            path = self.manifest.parent / "src" / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(SOURCE if relative == "lib.rs" else "", encoding="utf-8")

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def check(self) -> None:
        validate(self.manifest, self.build, self.capture_manifest, self.capture_build, self.facades, self.document, self.event_file)

    def test_narrow_pack_control_passes(self) -> None:
        self.check()

    def test_mutation_capture_dependency_alias_is_rejected(self) -> None:
        self.manifest.write_text(MANIFEST + '\ncapture_alias = { package = "ctx-history-capture", path = "../ctx-history-capture" }\n', encoding="utf-8")
        with self.assertRaisesRegex(BoundaryError, "forbidden Cargo dependency"):
            self.check()

    def test_mutation_cross_pack_bazel_dependency_is_rejected(self) -> None:
        self.build.write_text(BUILD + '\n"//crates/ctx-history-provider-gemini:lib",\n', encoding="utf-8")
        with self.assertRaisesRegex(BoundaryError, "Bazel dependency inventory|authority"):
            self.check()

    def test_mutation_capture_authority_copy_is_rejected(self) -> None:
        (self.manifest.parent / "src/lib.rs").write_text(SOURCE + "\nCaptureProviderRuntime\n", encoding="utf-8")
        with self.assertRaisesRegex(BoundaryError, "capture/index/selector authority"):
            self.check()

    def test_mutation_selector_policy_copy_is_rejected(self) -> None:
        (self.manifest.parent / "src/lib.rs").write_text(SOURCE + "\ndocument_leaf_execution_policy(\n", encoding="utf-8")
        with self.assertRaisesRegex(BoundaryError, "capture/index/selector authority"):
            self.check()

    def test_mutation_local_nanoclaw_checkpoint_wire_decoder_is_rejected(self) -> None:
        (self.manifest.parent / "src/lib.rs").write_text(SOURCE + "\nNANOCLAW_DOCUMENT_FRONTIER_KIND\n", encoding="utf-8")
        with self.assertRaisesRegex(BoundaryError, "capture/index/selector authority"):
            self.check()

    def test_mutation_public_auggie_selection_is_rejected(self) -> None:
        (self.manifest.parent / "src/lib.rs").write_text(SOURCE + "\npub enum AuggieTreeSelection\n", encoding="utf-8")
        with self.assertRaisesRegex(BoundaryError, "exposed Auggie"):
            self.check()

    def test_mutation_nanoclaw_capture_runtime_binding_is_required(self) -> None:
        self.document.write_text(
            "NanoClawDocumentTreeAdapter::new_with_base_sources", encoding="utf-8"
        )
        with self.assertRaisesRegex(BoundaryError, "NanoClaw registration"):
            self.check()

    def test_mutation_openhands_capture_runtime_binding_is_required(self) -> None:
        self.event_file.write_text("OpenHandsEventFileAdapterV2", encoding="utf-8")
        with self.assertRaisesRegex(BoundaryError, "OpenHands registration"):
            self.check()


def load_tests(
    loader: unittest.TestLoader,
    tests: unittest.TestSuite,
    pattern: str | None,
) -> unittest.TestSuite:
    del pattern
    test_names = set(loader.getTestCaseNames(DocumentProjectionBoundaryMutationTests))
    mutation_names = {
        name for name in test_names if name.startswith("test_mutation_")
    }
    if len(mutation_names) != EXPECTED_ADVERSARIAL_MUTATION_COUNT:
        raise AssertionError(
            "document-projection adversarial mutation count drifted: "
            f"expected={EXPECTED_ADVERSARIAL_MUTATION_COUNT} actual={len(mutation_names)}"
        )
    controls = test_names - mutation_names
    if controls != {"test_narrow_pack_control_passes"}:
        raise AssertionError(
            "document-projection positive control roster drifted: "
            + ", ".join(sorted(controls))
        )
    return tests


if __name__ == "__main__":
    unittest.main()
