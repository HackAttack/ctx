#!/usr/bin/env python3
"""Enforce Trae's lower-only pack and capture-owned composition boundary."""

from __future__ import annotations

import argparse
import sys
import tomllib
from pathlib import Path


class BoundaryError(RuntimeError):
    pass


EXPECTED_DEPENDENCIES = {
    "chrono",
    "ctx-history-capture-model",
    "ctx-history-capture-runtime",
    "ctx-history-core",
    "ctx-history-provider-runtime",
    "ctx-history-source-io",
    "ctx-history-source-sqlite",
    "rusqlite",
    "serde",
    "serde_json",
    "sha2",
    "thiserror",
}
EXPECTED_DEV_DEPENDENCIES = {"tempfile"}
EXPECTED_PATHS = {
    dependency: f"../{dependency}"
    for dependency in EXPECTED_DEPENDENCIES
    if dependency.startswith("ctx-history-")
}
REQUIRED_BUILD_LABELS = {
    "//crates/ctx-history-capture-model:lib",
    "//crates/ctx-history-capture-runtime:lib",
    "//crates/ctx-history-core:lib",
    "//crates/ctx-history-provider-runtime:lib",
    "//crates/ctx-history-source-io:lib",
    "//crates/ctx-history-source-sqlite:lib",
}
FORBIDDEN_PACK_FRAGMENTS = (
    "ctx_history_capture::",
    "ctx_history_index::",
    "ctx_history_index_format::",
    "CaptureDocumentLifecycle",
    "CaptureDocumentSpool",
    "CaptureProviderRuntime",
    "IndexCaptureLifecycle",
)


def _read_rust_tree(root_or_file: Path) -> str:
    root = root_or_file if root_or_file.is_dir() else root_or_file.parent
    return "\n".join(
        path.read_text(encoding="utf-8") for path in sorted(root.rglob("*.rs"))
    )


def _require_fragments(text: str, fragments: tuple[str, ...], label: str) -> None:
    missing = [fragment for fragment in fragments if fragment not in text]
    if missing:
        raise BoundaryError(f"{label} drifted: missing {', '.join(missing)}")


def validate(
    manifest: Path,
    build: Path,
    source_root: Path,
    facade: Path,
    registration: Path,
    discovery: Path,
) -> None:
    data = tomllib.loads(manifest.read_text(encoding="utf-8"))
    package = data.get("package", {})
    if package.get("name") != "ctx-history-provider-trae":
        raise BoundaryError("Trae pack package identity drifted")
    if package.get("version") != {"workspace": True}:
        raise BoundaryError("Trae pack must inherit version.workspace")

    dependencies = data.get("dependencies", {})
    dev_dependencies = data.get("dev-dependencies", {})
    if set(dependencies) != EXPECTED_DEPENDENCIES:
        raise BoundaryError(
            f"Trae Cargo dependency inventory drifted: {sorted(dependencies)}"
        )
    if set(dev_dependencies) != EXPECTED_DEV_DEPENDENCIES:
        raise BoundaryError(
            "Trae Cargo dev-dependency inventory drifted: "
            f"{sorted(dev_dependencies)}"
        )
    for dependency, expected_path in EXPECTED_PATHS.items():
        if dependencies[dependency] != {"path": expected_path}:
            raise BoundaryError(f"Trae path dependency drifted: {dependency}")

    build_text = build.read_text(encoding="utf-8")
    forbidden_labels = ("//crates/ctx-history-capture:", "//crates/ctx-history-index:")
    if any(label in build_text for label in forbidden_labels):
        raise BoundaryError("Trae pack gained capture/index Bazel authority")
    missing_labels = sorted(label for label in REQUIRED_BUILD_LABELS if label not in build_text)
    if missing_labels:
        raise BoundaryError("Trae lower-layer Bazel inventory drifted: " + ", ".join(missing_labels))
    _require_fragments(
        build_text,
        ('crate_name = "ctx_history_provider_trae"', 'name = "test_support_lib"'),
        "Trae Bazel targets",
    )

    pack_text = _read_rust_tree(source_root)
    retained = [fragment for fragment in FORBIDDEN_PACK_FRAGMENTS if fragment in pack_text]
    if retained:
        raise BoundaryError("Trae pack retained forbidden authority: " + ", ".join(retained))
    _require_fragments(
        pack_text,
        (
            "ProviderRuntimeBinding",
            "ReplacementDocumentTree",
            "ProviderChangedDocumentSink",
            "TRAE_CHAT_ROWS_QUERY",
            "TRAE_CHAT_KEYS",
        ),
        "Trae production implementation",
    )

    facade_text = facade.read_text(encoding="utf-8")
    _require_fragments(
        facade_text,
        (
            "pub(crate) use ctx_history_provider_trae",
            "ctx_history_provider_trae::TraeReplacementTree<",
            "CaptureProviderRuntime",
        ),
        "capture Trae facade",
    )
    duplicate_root = facade.with_suffix("")
    if duplicate_root.exists() and any(duplicate_root.rglob("*.rs")):
        raise BoundaryError("capture retained duplicate Trae production implementation")

    registration_text = registration.read_text(encoding="utf-8")
    _require_fragments(
        registration_text,
        (
            "fn register_trae_route",
            "TraeReplacementTree::new(data_root, source.path.clone())",
            "register_replacement_document_tree_route_with_authority",
            "SourceBackedSelectorAuthority::DiscoveredWinner",
        ),
        "capture Trae route registration",
    )

    discovery_text = discovery.read_text(encoding="utf-8")
    _require_fragments(
        discovery_text,
        (
            "TraeProbeFragment::new",
            "classify_trae_payload_for_discovery",
            "trae_payload_admission",
            "TRAE_CHAT_ROWS_QUERY",
        ),
        "capture Trae discovery probe",
    )


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("manifest", type=Path)
    parser.add_argument("build", type=Path)
    parser.add_argument("source_root", type=Path)
    parser.add_argument("facade", type=Path)
    parser.add_argument("registration", type=Path)
    parser.add_argument("discovery", type=Path)
    args = parser.parse_args(argv)
    try:
        validate(
            args.manifest,
            args.build,
            args.source_root,
            args.facade,
            args.registration,
            args.discovery,
        )
    except (BoundaryError, OSError, tomllib.TOMLDecodeError) as error:
        print(error, file=sys.stderr)
        return 1
    print("ctx-history-provider-trae ownership boundary ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
