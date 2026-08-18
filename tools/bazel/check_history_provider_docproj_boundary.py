#!/usr/bin/env python3
"""Fail closed on the Auggie/NanoClaw/OpenHands provider-pack boundary."""

from __future__ import annotations

import argparse
import re
import sys
import tomllib
from pathlib import Path
from typing import Any


class BoundaryError(RuntimeError):
    pass


EXPECTED_DEPENDENCIES: dict[str, Any] = {
    "chrono": {"workspace": True},
    "ctx-history-capture-model": {"path": "../ctx-history-capture-model"},
    "ctx-history-capture-runtime": {"path": "../ctx-history-capture-runtime"},
    "ctx-history-core": {"path": "../ctx-history-core"},
    "ctx-history-provider-runtime": {"path": "../ctx-history-provider-runtime"},
    "ctx-history-source-discovery": {"path": "../ctx-history-source-discovery"},
    "ctx-history-source-io": {"path": "../ctx-history-source-io"},
    "ctx-history-source-sqlite": {"path": "../ctx-history-source-sqlite"},
    "rusqlite": {"workspace": True},
    "serde": {"workspace": True},
    "serde_json": {"workspace": True},
    "sha2": {"workspace": True},
    "thiserror": {"workspace": True},
}
EXPECTED_DEV_DEPENDENCIES: dict[str, Any] = {
    "ctx-history-source-io": {
        "path": "../ctx-history-source-io",
        "features": ["test-support"],
    },
    "ctx-history-source-sqlite": {
        "path": "../ctx-history-source-sqlite",
        "features": ["test-support"],
    },
    "tempfile": {"workspace": True},
}
EXPECTED_INTERNAL_BAZEL = {
    "//crates/ctx-history-capture-model:lib",
    "//crates/ctx-history-capture-runtime:lib",
    "//crates/ctx-history-core:lib",
    "//crates/ctx-history-provider-runtime:lib",
    "//crates/ctx-history-provider-runtime:test_support_lib",
    "//crates/ctx-history-source-discovery:lib",
    "//crates/ctx-history-source-io:lib",
    "//crates/ctx-history-source-io:test_support_lib",
    "//crates/ctx-history-source-sqlite:lib",
    "//crates/ctx-history-source-sqlite:test_support_lib",
}
OWNED_PROVIDERS = ("auggie", "nanoclaw", "openhands")
FORBIDDEN_PACKAGES = {
    "ctx-history-capture", "ctx-history-index", "ctx-history-index-format",
    "ctx-history-index-generation", "ctx-history-index-query",
    "ctx-history-provider-gemini", "ctx-history-provider-mistral-mux",
    "ctx-history-providers-jsonl-shared",
}


def _read(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def _dependency_tables(manifest: dict[str, Any]) -> list[dict[str, Any]]:
    tables = [
        manifest.get("dependencies", {}), manifest.get("dev-dependencies", {}),
        manifest.get("build-dependencies", {}),
    ]
    for target in manifest.get("target", {}).values():
        tables.extend(target.get(name, {}) for name in ("dependencies", "dev-dependencies", "build-dependencies"))
    return [table for table in tables if isinstance(table, dict)]


def validate_manifest(path: Path) -> None:
    with path.open("rb") as handle:
        manifest = tomllib.load(handle)
    if manifest.get("package", {}).get("name") != "ctx-history-provider-docproj":
        raise BoundaryError("document-projection package identity drifted")
    for table in _dependency_tables(manifest):
        for alias, spec in table.items():
            package = spec.get("package", alias) if isinstance(spec, dict) else alias
            if package in FORBIDDEN_PACKAGES:
                raise BoundaryError(f"document-projection pack has forbidden Cargo dependency: {package}")
    if manifest.get("dependencies") != EXPECTED_DEPENDENCIES:
        raise BoundaryError("document-projection production dependency inventory drifted")
    if manifest.get("dev-dependencies") != EXPECTED_DEV_DEPENDENCIES:
        raise BoundaryError("document-projection test dependency inventory drifted")
    if manifest.get("build-dependencies") or manifest.get("target"):
        raise BoundaryError("document-projection dependency-table bypass")


def validate_build(path: Path) -> None:
    source = _read(path)
    internal = set(re.findall(r'"(//crates/ctx-history-[^"\s]+:[^"\s]+)"', source))
    if internal != EXPECTED_INTERNAL_BAZEL:
        raise BoundaryError("document-projection Bazel dependency inventory drifted")
    if "tempfile" in source:
        raise BoundaryError("document-projection Bazel production surface gained tempfile")
    for package in FORBIDDEN_PACKAGES:
        if f"//crates/{package}:" in source:
            raise BoundaryError(f"document-projection Bazel graph gained {package} authority")


def validate_sources(manifest: Path) -> None:
    root = manifest.parent / "src"
    paths = {path.relative_to(root).as_posix() for path in root.rglob("*.rs")}
    missing_entries = [
        f"providers/{provider}.rs"
        for provider in OWNED_PROVIDERS
        if f"providers/{provider}.rs" not in paths
    ]
    unexpected_provider_sources = sorted(
        path
        for path in paths
        if path.startswith("providers/")
        and path != "providers/mod.rs"
        and not any(
            path == f"providers/{provider}.rs"
            or path.startswith(f"providers/{provider}/")
            for provider in OWNED_PROVIDERS
        )
    )
    if missing_entries or unexpected_provider_sources:
        raise BoundaryError(
            "document-projection source ownership drifted: "
            f"missing={missing_entries} unexpected={unexpected_provider_sources}"
        )
    source = "\n".join(_read(root / path) for path in sorted(paths))
    required = (
        "decode_document_full_snapshot_checkpoint", "DocumentFullSnapshotCheckpointError",
        "DocumentLeafExecutionPolicy::Serial", "DocumentLeafExecutionPolicy::Independent",
        "AUGGIE_SESSION_JSON_SOURCE_FORMAT", "NANOCLAW_SOURCE_FORMAT",
        "OPENHANDS_FILE_EVENTS_SOURCE_FORMAT", "ProviderRuntimeBinding",
        "ReplacementDocumentTree", "ProviderChangedDocumentSink",
    )
    missing = [fragment for fragment in required if fragment not in source]
    if missing:
        raise BoundaryError("document-projection provider surface is incomplete: " + ", ".join(missing))
    forbidden = (
        "ctx_history_capture::", "ctx_history_index::", "CaptureProviderRuntime",
        "IndexCaptureLifecycle", "SourceBackedProviderRegistry",
        "SourceBackedSelectorAuthority", "document_leaf_execution_policy(",
        "NANOCLAW_DOCUMENT_FRONTIER_KIND",
    )
    retained = [fragment for fragment in forbidden if fragment in source]
    if retained:
        raise BoundaryError("document-projection pack gained capture/index/selector authority: " + ", ".join(retained))
    if "pub enum AuggieTreeSelection" in source:
        raise BoundaryError("document-projection exposed Auggie tree selection")


def _depends_on(manifest: dict[str, Any], package: str) -> bool:
    for table in _dependency_tables(manifest):
        for alias, spec in table.items():
            resolved = spec.get("package", alias) if isinstance(spec, dict) else alias
            if resolved == package:
                return True
    return False


def validate_capture_cleanup(capture_manifest: Path, capture_build: Path) -> None:
    with capture_manifest.open("rb") as handle:
        manifest = tomllib.load(handle)
    if _depends_on(manifest, "ctx-history-provider-docproj"):
        raise BoundaryError("capture Cargo surface unexpectedly owns document-projection pack")
    if '"//crates/ctx-history-provider-docproj:lib"' in _read(capture_build):
        raise BoundaryError("capture Bazel surface unexpectedly owns document-projection pack")


def validate_composition_ownership(composition_manifest: Path, composition_build: Path) -> None:
    with composition_manifest.open("rb") as handle:
        manifest = tomllib.load(handle)
    if manifest.get("dependencies", {}).get("ctx-history-provider-docproj") != {"path": "../ctx-history-provider-docproj"}:
        raise BoundaryError("capture composition does not depend on document-projection pack")
    if '"//crates/ctx-history-provider-docproj:lib"' not in _read(composition_build):
        raise BoundaryError("capture composition Bazel graph does not depend on document-projection pack")


def validate_capture_composition(
    capture_manifest: Path,
    capture_build: Path,
    composition_manifest: Path,
    composition_build: Path,
    facades: Path,
    document_registration: Path,
    event_file_registration: Path,
) -> None:
    validate_capture_cleanup(capture_manifest, capture_build)
    validate_composition_ownership(composition_manifest, composition_build)
    facade = _read(facades)
    for fragment in ("pub(crate) mod nanoclaw;", "pub(crate) mod openhands;"):
        if fragment not in facade:
            raise BoundaryError("capture provider facade roster drifted: " + fragment)
    document = _read(document_registration)
    if "NanoClawDocumentTreeAdapter::<CaptureProviderRuntime>::new_with_base_sources" not in document:
        raise BoundaryError("capture NanoClaw registration no longer binds the provider runtime")
    event_file = _read(event_file_registration)
    if "OpenHandsEventFileAdapterV2::<CaptureProviderRuntime>" not in event_file:
        raise BoundaryError("capture OpenHands registration no longer binds the provider runtime")


def validate(manifest: Path, build: Path, capture_manifest: Path, capture_build: Path, composition_manifest: Path, composition_build: Path, facades: Path, document_registration: Path, event_file_registration: Path) -> None:
    validate_manifest(manifest)
    validate_build(build)
    validate_sources(manifest)
    validate_capture_composition(capture_manifest, capture_build, composition_manifest, composition_build, facades, document_registration, event_file_registration)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser()
    for name in ("manifest", "build", "capture_manifest", "capture_build", "composition_manifest", "composition_build", "facades", "document_registration", "event_file_registration"):
        parser.add_argument(name, type=Path)
    args = parser.parse_args(argv)
    try:
        validate(**vars(args))
    except (BoundaryError, OSError, tomllib.TOMLDecodeError) as error:
        print(error, file=sys.stderr)
        return 1
    print("ctx-history-provider-docproj dependency/ownership boundary ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
