#!/usr/bin/env python3
from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from check_history_provider_runtime_boundary import BoundaryError, validate


MANIFEST = """\
[package]
name = "ctx-history-provider-runtime"

[dependencies]
chrono.workspace = true
ctx-history-capture-model = { path = "../ctx-history-capture-model" }
ctx-history-capture-runtime = { path = "../ctx-history-capture-runtime" }
ctx-history-core = { path = "../ctx-history-core" }
ctx-history-jsonl = { path = "../ctx-history-jsonl" }
ctx-history-source-io = { path = "../ctx-history-source-io" }
ctx-history-source-sqlite = { path = "../ctx-history-source-sqlite" }
rusqlite.workspace = true
serde.workspace = true
serde_json.workspace = true
sha2.workspace = true
thiserror.workspace = true
uuid.workspace = true
"""

BUILD = "\n".join(
    f'    "//crates/{name}:lib",'
    for name in (
        "ctx-history-capture-model",
        "ctx-history-capture-runtime",
        "ctx-history-core",
        "ctx-history-jsonl",
        "ctx-history-source-io",
        "ctx-history-source-sqlite",
    )
) + """
    "//crates/ctx-history-jsonl:test_support_lib",
    "//crates/ctx-history-source-io:test_support_lib",
    "//crates/ctx-history-source-sqlite:test_support_lib",
"""

RUNTIME = """\
pub type ProviderJsonlInventoryLimit = ctx_history_source_io::ProviderJsonlInventoryLimit;
pub trait ProviderRuntimeBinding {
    type CaptureLifecycleSink: CaptureLifecycleSink;
    type DocumentRecordSpool: DocumentRecordSpool;
}
type WorkerServices = ();
pub trait ProviderRouteRegistrar {}
pub struct ProviderRouteControlExpectation;
"""

SOURCE_SQLITE_LIB = "pub use value::NativeSqliteValue;\n"
SOURCE_SQLITE_VALUE = "pub enum NativeSqliteValue { Null }\n"
JSONL_RUNTIME = """\
pub type ProviderJsonlReader = ctx_history_jsonl::JsonlReader<CaptureError>;
pub type ProviderJsonlPhysicalStream = ctx_history_jsonl::JsonlPhysicalStream<CaptureError>;
pub type ProviderJsonlLeaf = ctx_history_jsonl::JsonlFamilyLeaf<CaptureError>;
pub type ProviderJsonlInventory = ctx_history_jsonl::JsonlFamilyInventory<CaptureError>;
pub type ProviderJsonlMembershipObservation =
    ctx_history_jsonl::JsonlFamilyMembershipObservation<CaptureError>;
pub type ProviderJsonlTerminalProof = ctx_history_jsonl::JsonlFamilyTerminalProof<CaptureError>;
pub type ProviderJsonlOptimizedLeafOutcome =
    ctx_history_jsonl::JsonlFamilyOptimizedLeafOutcome<CaptureError>;
pub type ProviderJsonlWorkerContext<B> =
    ctx_history_jsonl::JsonlFamilyWorkerContext<ProviderJsonlRuntime<B>>;
pub type ProviderJsonlExecutionIo<B> =
    ctx_history_jsonl::JsonlFamilyExecutionIo<ProviderJsonlRuntime<B>>;
pub type ProviderJsonlAdapter<B> = dyn JsonlFamilyAdapter<Runtime = ProviderJsonlRuntime<B>>;
pub use ctx_history_jsonl::{fit_jsonl_activity, JsonlActivityObservedBytes};
pub fn encode_bounded_checkpoint() {}
pub fn decode_bounded_checkpoint() {}
pub fn probe_first_record() {}
pub fn probe_records_until() {}
pub fn provider_jsonl_family_driver<B: ProviderRuntimeBinding>() {}
"""

COMPILE_FIXTURE = """\
use ctx_history_provider_runtime::{
    provider_jsonl_family_driver, ProviderJsonlAdapter, ProviderJsonlExecutionIo,
    ProviderJsonlInventory, ProviderJsonlLeaf, ProviderJsonlPhysicalStream, ProviderJsonlReader,
    ProviderJsonlRouteDriver, ProviderJsonlWorkerContext, ProviderRuntimeBinding,
};

struct FakeBinding;
struct FakeLifecycle;
struct FakeSpool;

impl ProviderRuntimeBinding for FakeBinding {
    type CaptureLifecycleSink = FakeLifecycle;
    type DocumentRecordSpool = FakeSpool;
}

type _Driver = ProviderJsonlRouteDriver<FakeBinding>;
type _Reader = ProviderJsonlReader;
type _Stream = ProviderJsonlPhysicalStream;
type _Leaf = ProviderJsonlLeaf;
type _Inventory = ProviderJsonlInventory;
type _Worker = ProviderJsonlWorkerContext<FakeBinding>;
type _ExecutionIo = ProviderJsonlExecutionIo<FakeBinding>;

fn _compile_proof(adapter: std::sync::Arc<ProviderJsonlAdapter<FakeBinding>>) {
    let _ = provider_jsonl_family_driver::<FakeBinding>(adapter, std::path::PathBuf::new());
}
"""

CAPTURE = """\
pub struct CaptureProviderRuntime;
impl ProviderRuntimeBinding for CaptureProviderRuntime {
    type CaptureLifecycleSink = super::IndexCaptureLifecycle;
    type DocumentRecordSpool = document::CaptureDocumentSpool;
}
"""
CAPTURE_SOURCE_BACKED = """\
pub(crate) use family::jsonl::FallbackEventIdentityState;
"""
CAPTURE_JSONL_COMPAT = """\
pub(crate) type FallbackEventIdentityState =
    ctx_history_provider_runtime::ProviderFallbackEventIdentityState<super::CaptureProviderRuntime>;
"""
SHARED_JSONL_ERROR = """\
pub use ctx_history_provider_runtime::{CaptureError, ProviderJsonlInventoryLimit, Result};
"""


class ProviderRuntimeBoundaryMutationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        root = Path(self.temporary.name)
        self.manifest = root / "Cargo.toml"
        self.build = root / "BUILD.bazel"
        self.adapter = root / "adapter.rs"
        self.error = root / "error.rs"
        self.runtime = root / "lib.rs"
        self.jsonl = root / "jsonl.rs"
        self.record = root / "record.rs"
        self.route = root / "route.rs"
        self.source_io = root / "source_io.rs"
        self.sqlite = root / "sqlite.rs"
        self.compile_fixture = root / "provider_pack_jsonl_compile.rs"
        self.shared_jsonl_error = root / "shared-jsonl-error.rs"
        self.capture = root / "family.rs"
        self.capture_source_backed = root / "source_backed.rs"
        self.capture_jsonl_compat = root / "jsonl_compat.rs"
        self.source_sqlite_lib = root / "source-sqlite-lib.rs"
        self.source_sqlite_value = root / "value.rs"
        self.manifest.write_text(MANIFEST, encoding="utf-8")
        self.build.write_text(BUILD, encoding="utf-8")
        self.runtime.write_text(RUNTIME, encoding="utf-8")
        self.adapter.write_text("", encoding="utf-8")
        self.error.write_text("", encoding="utf-8")
        self.jsonl.write_text(JSONL_RUNTIME, encoding="utf-8")
        self.record.write_text("", encoding="utf-8")
        self.route.write_text(RUNTIME, encoding="utf-8")
        self.source_io.write_text("", encoding="utf-8")
        self.sqlite.write_text("", encoding="utf-8")
        self.compile_fixture.write_text(COMPILE_FIXTURE, encoding="utf-8")
        self.shared_jsonl_error.write_text(SHARED_JSONL_ERROR, encoding="utf-8")
        self.capture.write_text(CAPTURE, encoding="utf-8")
        self.capture_source_backed.write_text(CAPTURE_SOURCE_BACKED, encoding="utf-8")
        self.capture_jsonl_compat.write_text(CAPTURE_JSONL_COMPAT, encoding="utf-8")
        self.source_sqlite_lib.write_text(SOURCE_SQLITE_LIB, encoding="utf-8")
        self.source_sqlite_value.write_text(SOURCE_SQLITE_VALUE, encoding="utf-8")

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def validate(self) -> None:
        validate(
            self.manifest,
            self.build,
            [
                self.adapter,
                self.error,
                self.runtime,
                self.jsonl,
                self.record,
                self.route,
                self.source_io,
                self.sqlite,
            ],
            self.compile_fixture,
            self.shared_jsonl_error,
            self.capture,
            self.capture_source_backed,
            self.capture_jsonl_compat,
            self.source_sqlite_lib,
            self.source_sqlite_value,
        )

    def test_narrow_seam_passes(self) -> None:
        self.validate()

    def test_renamed_index_dependency_is_rejected(self) -> None:
        self.manifest.write_text(
            MANIFEST
            + '\nindex_alias = { package = "ctx-history-index", path = "../ctx-history-index" }\n',
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "forbidden Cargo dependency"):
            self.validate()

    def test_capture_bazel_dependency_is_rejected(self) -> None:
        self.build.write_text(
            BUILD + '\n"//crates/ctx-history-capture:lib"\n', encoding="utf-8"
        )
        with self.assertRaisesRegex(BoundaryError, "Bazel dependencies drifted"):
            self.validate()

    def test_concrete_index_lifecycle_is_rejected_below_seam(self) -> None:
        self.error.write_text("struct IndexCaptureLifecycle;\n", encoding="utf-8")
        with self.assertRaisesRegex(BoundaryError, "concrete capture authority"):
            self.validate()

    def test_compile_fixture_cannot_import_capture(self) -> None:
        self.compile_fixture.write_text(
            COMPILE_FIXTURE + "\nuse ctx_history_capture::CaptureError;\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "compile fixture gained capture/index imports"):
            self.validate()

    def test_compile_fixture_cannot_import_index(self) -> None:
        self.compile_fixture.write_text(
            COMPILE_FIXTURE + "\nuse ctx_history_index::IndexError;\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "compile fixture gained capture/index imports"):
            self.validate()

    def test_runtime_cannot_mirror_source_io_inventory_limit(self) -> None:
        self.runtime.write_text(
            RUNTIME.replace(
                "pub type ProviderJsonlInventoryLimit = ctx_history_source_io::ProviderJsonlInventoryLimit;",
                "pub enum ProviderJsonlInventoryLimit { EligiblePaths }",
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(
            BoundaryError, "mirrored source-io inventory-limit authority"
        ):
            self.validate()

    def test_shared_jsonl_cannot_reclaim_capture_error_authority(self) -> None:
        self.shared_jsonl_error.write_text(
            SHARED_JSONL_ERROR + "pub enum CaptureError { InvalidPayload(String) }\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "local error classification authority"):
            self.validate()

    def test_runtime_cannot_drop_generic_jsonl_execution_io_alias(self) -> None:
        self.jsonl.write_text(
            JSONL_RUNTIME.replace(
                "pub type ProviderJsonlExecutionIo<B> =\n    ctx_history_jsonl::JsonlFamilyExecutionIo<ProviderJsonlRuntime<B>>;\n",
                "",
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "contract is incomplete"):
            self.validate()

    def test_capture_binding_cannot_drop_the_concrete_spool(self) -> None:
        self.capture.write_text(
            CAPTURE.replace(
                "type DocumentRecordSpool = document::CaptureDocumentSpool;", ""
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "capture composition binding drifted"):
            self.validate()

    def test_capture_fallback_identity_binding_cannot_be_test_only(self) -> None:
        self.capture_source_backed.write_text(
            "#[cfg(test)]\npub(crate) use family::jsonl::FallbackEventIdentityState;\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "became test-only"):
            self.validate()

    def test_capture_cannot_recreate_fallback_identity_authority(self) -> None:
        self.capture_jsonl_compat.write_text(
            "pub(crate) struct FallbackEventIdentityState;\n", encoding="utf-8"
        )
        with self.assertRaisesRegex(BoundaryError, "authority drifted"):
            self.validate()

    def test_source_sqlite_cannot_drop_native_sqlite_value_authority(self) -> None:
        self.source_sqlite_value.write_text(
            "pub struct OtherValue;\n", encoding="utf-8"
        )
        with self.assertRaisesRegex(BoundaryError, "no longer owns"):
            self.validate()

    def test_mutation_inventory_counts_are_exact(self) -> None:
        baseline = 1
        negative = 12
        discovered = {
            name
            for name in dir(type(self))
            if name.startswith("test_")
            and name
            not in {
                "test_narrow_seam_passes",
                "test_mutation_inventory_counts_are_exact",
            }
        }
        self.assertEqual(
            len(discovered),
            negative,
            f"provider-runtime mutation inventory drifted: expected {negative}, found {len(discovered)}",
        )
        self.assertEqual(baseline, 1)


if __name__ == "__main__":
    unittest.main()
