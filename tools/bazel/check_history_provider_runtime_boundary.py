#!/usr/bin/env python3
"""Exact dependency and ownership boundary for history provider-runtime."""

from __future__ import annotations

import argparse
import re
import sys
import tomllib
from pathlib import Path
from typing import Any


EXPECTED_INTERNAL_CARGO = {
    "ctx-history-capture-model",
    "ctx-history-capture-runtime",
    "ctx-history-core",
    "ctx-history-jsonl",
    "ctx-history-source-io",
    "ctx-history-source-sqlite",
}
EXPECTED_DEPENDENCIES: dict[str, Any] = {
    "chrono": {"workspace": True},
    "ctx-history-capture-model": {"path": "../ctx-history-capture-model"},
    "ctx-history-capture-runtime": {"path": "../ctx-history-capture-runtime"},
    "ctx-history-core": {"path": "../ctx-history-core"},
    "ctx-history-jsonl": {"path": "../ctx-history-jsonl"},
    "ctx-history-source-io": {"path": "../ctx-history-source-io"},
    "ctx-history-source-sqlite": {"path": "../ctx-history-source-sqlite"},
    "rusqlite": {"workspace": True},
    "serde": {"workspace": True},
    "serde_json": {"workspace": True},
    "sha2": {"workspace": True},
    "thiserror": {"workspace": True},
    "uuid": {"workspace": True},
}
EXPECTED_INTERNAL_BAZEL = {
    f"//crates/{name}:lib" for name in EXPECTED_INTERNAL_CARGO
} | {
    "//crates/ctx-history-jsonl:test_support_lib",
    "//crates/ctx-history-source-io:test_support_lib",
    "//crates/ctx-history-source-sqlite:test_support_lib",
}
FORBIDDEN_PACKAGES = {"ctx-history-capture", "ctx-history-index"}
REQUIRED_RUNTIME_SURFACE = (
    "pub type ProviderJsonlInventoryLimit = ctx_history_source_io::ProviderJsonlInventoryLimit;",
    "pub trait ProviderRuntimeBinding",
    "type CaptureLifecycleSink: CaptureLifecycleSink;",
    "type DocumentRecordSpool: DocumentRecordSpool;",
    "pub trait ProviderRouteRegistrar",
    "pub struct ProviderRouteControlExpectation",
    "pub type ProviderJsonlReader",
    "pub type ProviderJsonlPhysicalStream",
    "pub type ProviderJsonlLeaf",
    "pub type ProviderJsonlInventory",
    "pub type ProviderJsonlMembershipObservation",
    "pub type ProviderJsonlTerminalProof",
    "pub type ProviderJsonlOptimizedLeafOutcome",
    "pub type ProviderJsonlWorkerContext<B>",
    "pub type ProviderJsonlExecutionIo<B>",
    "pub type ProviderJsonlAdapter<B>",
    "type WorkerServices = ();",
    "fit_jsonl_activity",
    "JsonlActivityObservedBytes",
    "pub fn encode_bounded_checkpoint",
    "pub fn decode_bounded_checkpoint",
    "pub fn probe_first_record",
    "pub fn probe_records_until",
    "pub fn provider_jsonl_family_driver<B: ProviderRuntimeBinding>",
)
FORBIDDEN_RUNTIME_SOURCE_FRAGMENTS = (
    "ctx_history_capture::",
    "ctx_history_index::",
    "IndexCaptureLifecycle",
    "DeferredCoreRecords",
    "SourceBackedProviderRegistry",
    "fit_jsonl_mcp_exchange",
    "JsonlMcpObservedEncodedBytes",
)
REQUIRED_COMPILE_FIXTURE_SURFACE = (
    "provider_jsonl_family_driver::<FakeBinding>",
    "ProviderJsonlRouteDriver<FakeBinding>",
    "ProviderJsonlAdapter<FakeBinding>",
    "impl ProviderRuntimeBinding for FakeBinding",
)
FORBIDDEN_COMPILE_FIXTURE_FRAGMENTS = (
    "ctx_history_capture::",
    "ctx_history_index::",
)
SHARED_JSONL_ERROR_BINDING = (
    "pub use ctx_history_provider_runtime::{CaptureError, ProviderJsonlInventoryLimit, Result};"
)
FORBIDDEN_SHARED_JSONL_ERROR_FRAGMENTS = (
    "enum CaptureError",
    "enum ProviderJsonlInventoryLimit",
    "impl JsonlFamilyError for CaptureError",
)


class BoundaryError(RuntimeError):
    pass


def _load(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def _dependency_tables(manifest: dict[str, Any]) -> list[dict[str, Any]]:
    tables = [manifest.get("dependencies", {})]
    tables.extend(
        manifest.get(name, {}) for name in ("dev-dependencies", "build-dependencies")
    )
    for target in manifest.get("target", {}).values():
        tables.extend(
            target.get(name, {})
            for name in ("dependencies", "dev-dependencies", "build-dependencies")
        )
    return [table for table in tables if isinstance(table, dict)]


def validate_manifest(path: Path) -> None:
    with path.open("rb") as handle:
        manifest = tomllib.load(handle)
    package = manifest.get("package", {})
    if package.get("name") != "ctx-history-provider-runtime":
        raise BoundaryError("provider-runtime package identity drifted")

    dependencies = manifest.get("dependencies", {})
    for table in _dependency_tables(manifest):
        for alias, specification in table.items():
            package_name = (
                specification.get("package", alias)
                if isinstance(specification, dict)
                else alias
            )
            if package_name in FORBIDDEN_PACKAGES:
                raise BoundaryError(
                    f"provider-runtime has forbidden Cargo dependency: {package_name}"
                )

    if dependencies != EXPECTED_DEPENDENCIES:
        raise BoundaryError(
            "provider-runtime Cargo dependency inventory drifted: "
            f"missing={sorted(set(EXPECTED_DEPENDENCIES) - set(dependencies))} "
            f"extra={sorted(set(dependencies) - set(EXPECTED_DEPENDENCIES))}"
        )
    internal = {name for name in dependencies if name.startswith("ctx-")}
    if internal != EXPECTED_INTERNAL_CARGO:
        raise BoundaryError(
            "provider-runtime internal Cargo dependencies drifted: "
            f"missing={sorted(EXPECTED_INTERNAL_CARGO - internal)} "
            f"extra={sorted(internal - EXPECTED_INTERNAL_CARGO)}"
        )

    if manifest.get("dev-dependencies") or manifest.get("build-dependencies"):
        raise BoundaryError("provider-runtime dependency-table bypass")
    if manifest.get("target"):
        raise BoundaryError("provider-runtime target-specific dependency bypass")


def validate_build(path: Path) -> None:
    source = _load(path)
    internal = set(re.findall(r'"(//crates/ctx-history-[^"\s]+:[^"\s]+)"', source))
    if internal != EXPECTED_INTERNAL_BAZEL:
        raise BoundaryError(
            "provider-runtime internal Bazel dependencies drifted: "
            f"missing={sorted(EXPECTED_INTERNAL_BAZEL - internal)} "
            f"extra={sorted(internal - EXPECTED_INTERNAL_BAZEL)}"
        )
    if "//crates/ctx-history-capture:" in source or "//crates/ctx-history-index:" in source:
        raise BoundaryError("provider-runtime Bazel graph gained capture/index authority")


def validate_runtime_source(paths: list[Path]) -> None:
    source = "\n".join(_load(path) for path in paths)
    missing = [fragment for fragment in REQUIRED_RUNTIME_SURFACE if fragment not in source]
    if missing:
        raise BoundaryError("provider-runtime contract is incomplete: " + ", ".join(missing))
    if "pub enum ProviderJsonlInventoryLimit" in source:
        raise BoundaryError(
            "provider-runtime regained mirrored source-io inventory-limit authority"
        )
    retained = [
        fragment for fragment in FORBIDDEN_RUNTIME_SOURCE_FRAGMENTS if fragment in source
    ]
    if retained:
        raise BoundaryError(
            "provider-runtime source gained concrete capture authority: "
            + ", ".join(retained)
        )


def validate_compile_fixture(path: Path) -> None:
    source = _load(path)
    missing = [
        fragment for fragment in REQUIRED_COMPILE_FIXTURE_SURFACE if fragment not in source
    ]
    if missing:
        raise BoundaryError(
            "provider-pack compile fixture drifted: " + ", ".join(missing)
        )
    retained = [
        fragment for fragment in FORBIDDEN_COMPILE_FIXTURE_FRAGMENTS if fragment in source
    ]
    if retained:
        raise BoundaryError(
            "provider-pack compile fixture gained capture/index imports: "
            + ", ".join(retained)
        )


def validate_shared_jsonl_error_binding(path: Path) -> None:
    source = _load(path)
    if SHARED_JSONL_ERROR_BINDING not in source:
        raise BoundaryError("shared-JSONL provider-runtime error binding drifted")
    retained = [
        fragment
        for fragment in FORBIDDEN_SHARED_JSONL_ERROR_FRAGMENTS
        if fragment in source
    ]
    if retained:
        raise BoundaryError(
            "shared-JSONL regained local error classification authority: "
            + ", ".join(retained)
        )


def validate_native_value_ownership(
    source_sqlite_lib: Path,
    source_sqlite_value: Path,
) -> None:
    sqlite_source = _load(source_sqlite_lib) + "\n" + _load(source_sqlite_value)
    if "pub enum NativeSqliteValue" not in sqlite_source:
        raise BoundaryError("source-sqlite no longer owns NativeSqliteValue")


def validate_capture_binding(path: Path) -> None:
    source = _load(path)
    required = (
        "pub struct CaptureProviderRuntime;",
        "type CaptureLifecycleSink = super::IndexCaptureLifecycle;",
        "type DocumentRecordSpool = document::CaptureDocumentSpool;",
    )
    missing = [fragment for fragment in required if fragment not in source]
    if missing:
        raise BoundaryError("capture composition binding drifted: " + ", ".join(missing))


def validate_capture_fallback_identity_binding(
    capture_source_backed: Path, capture_jsonl_compat: Path
) -> None:
    source_backed = _load(capture_source_backed)
    export = "pub(crate) use family::jsonl::FallbackEventIdentityState;"
    if export not in source_backed:
        raise BoundaryError("capture fallback identity production binding drifted")
    if re.search(
        r"#\[cfg\(test\)\]\s*pub\(crate\) use family::jsonl::FallbackEventIdentityState;",
        source_backed,
    ):
        raise BoundaryError("capture fallback identity binding became test-only")

    jsonl_compat = _load(capture_jsonl_compat)
    authority = (
        "pub(crate) type FallbackEventIdentityState =\n"
        "    ctx_history_provider_runtime::ProviderFallbackEventIdentityState<"
        "super::CaptureProviderRuntime>;"
    )
    if authority not in jsonl_compat:
        raise BoundaryError("capture fallback identity compatibility authority drifted")
    if re.search(r"(?:struct|enum) FallbackEventIdentityState\b", jsonl_compat):
        raise BoundaryError("capture recreated fallback identity authority")


def validate(
    manifest: Path,
    build: Path,
    runtime_sources: list[Path],
    compile_fixture: Path,
    shared_jsonl_error: Path,
    capture_binding: Path,
    capture_source_backed: Path,
    capture_jsonl_compat: Path,
    source_sqlite_lib: Path,
    source_sqlite_value: Path,
) -> None:
    validate_manifest(manifest)
    validate_build(build)
    validate_runtime_source(runtime_sources)
    validate_compile_fixture(compile_fixture)
    validate_shared_jsonl_error_binding(shared_jsonl_error)
    validate_capture_binding(capture_binding)
    validate_capture_fallback_identity_binding(
        capture_source_backed, capture_jsonl_compat
    )
    validate_native_value_ownership(source_sqlite_lib, source_sqlite_value)


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("manifest", type=Path)
    parser.add_argument("build", type=Path)
    parser.add_argument("runtime_sources", type=Path, nargs=8)
    parser.add_argument("compile_fixture", type=Path)
    parser.add_argument("shared_jsonl_error", type=Path)
    parser.add_argument("capture_binding", type=Path)
    parser.add_argument("capture_source_backed", type=Path)
    parser.add_argument("capture_jsonl_compat", type=Path)
    parser.add_argument("source_sqlite_lib", type=Path)
    parser.add_argument("source_sqlite_value", type=Path)
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    try:
        validate(
            args.manifest,
            args.build,
            args.runtime_sources,
            args.compile_fixture,
            args.shared_jsonl_error,
            args.capture_binding,
            args.capture_source_backed,
            args.capture_jsonl_compat,
            args.source_sqlite_lib,
            args.source_sqlite_value,
        )
    except (BoundaryError, OSError, tomllib.TOMLDecodeError) as error:
        print(error, file=sys.stderr)
        return 1
    print("ctx-history-provider-runtime dependency/ownership boundary ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
