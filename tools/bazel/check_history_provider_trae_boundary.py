#!/usr/bin/env python3
"""Enforce Trae's lower-only pack and composition-owned capture boundary."""

from __future__ import annotations

import argparse
import ast
import re
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
RUST_BLOCK_COMMENT = re.compile(r"/\*.*?\*/", re.DOTALL)
COMPOSITION_DEPS_ASSIGNMENT = re.compile(r"(?m)^COMPOSITION_DEPS[ \t]*=")
COMPOSITION_DEPS_LITERAL = re.compile(
    r"(?m)^COMPOSITION_DEPS[ \t]*=[ \t]*(\[[^\]]*\])[ \t]*(?:#.*)?$"
)


def _read_rust_tree(root_or_file: Path) -> str:
    root = root_or_file if root_or_file.is_dir() else root_or_file.parent
    return "\n".join(
        path.read_text(encoding="utf-8") for path in sorted(root.rglob("*.rs"))
    )


def _active_rust(text: str, label: str) -> str:
    code = RUST_BLOCK_COMMENT.sub("", text)
    if "/*" in code or "*/" in code:
        raise BoundaryError(f"{label} has malformed block comments")
    return "\n".join(line.split("//", 1)[0] for line in code.splitlines())


def _active_starlark_lines(text: str) -> set[str]:
    return {
        code.removesuffix(",")
        for line in text.splitlines()
        if (code := line.split("#", 1)[0].strip())
    }


def _composition_dependencies(text: str) -> list[str]:
    if len(COMPOSITION_DEPS_ASSIGNMENT.findall(text)) != 1:
        raise BoundaryError("composition production dependency inventory drifted")
    match = COMPOSITION_DEPS_LITERAL.search(text)
    if match is None:
        raise BoundaryError(
            "composition production dependency inventory is not literal"
        )
    try:
        dependencies = ast.literal_eval(match.group(1))
    except (SyntaxError, ValueError) as error:
        raise BoundaryError(
            "composition production dependency inventory is malformed"
        ) from error
    if not isinstance(dependencies, list) or not all(
        isinstance(dependency, str) for dependency in dependencies
    ):
        raise BoundaryError(
            "composition production dependency inventory is not literal"
        )
    return dependencies


def _has_exact_fragment(text: str, fragment: str) -> bool:
    prefix = r"(?<![A-Za-z0-9_])" if fragment[0].isalnum() else ""
    suffix = r"(?![A-Za-z0-9_])" if fragment[-1].isalnum() else ""
    return re.search(prefix + re.escape(fragment) + suffix, text) is not None


def _require_fragments(text: str, fragments: tuple[str, ...], label: str) -> None:
    missing = [
        fragment for fragment in fragments if not _has_exact_fragment(text, fragment)
    ]
    if missing:
        raise BoundaryError(f"{label} drifted: missing {', '.join(missing)}")


def _forbid_fragments(text: str, fragments: tuple[str, ...], label: str) -> None:
    retained = [
        fragment for fragment in fragments if _has_exact_fragment(text, fragment)
    ]
    if retained:
        raise BoundaryError(f"{label} retained forbidden authority: {', '.join(retained)}")


def validate(
    manifest: Path,
    build: Path,
    source_root: Path,
    capture_facade: Path,
    composition_manifest: Path,
    composition_build: Path,
    composition_facade: Path,
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

    build_lines = _active_starlark_lines(build.read_text(encoding="utf-8"))
    build_labels = {
        line[1:-1]
        for line in build_lines
        if len(line) >= 2 and line.startswith('"') and line.endswith('"')
    }
    forbidden_labels = ("//crates/ctx-history-capture:", "//crates/ctx-history-index:")
    if any(
        label.startswith(forbidden)
        for label in build_labels
        for forbidden in forbidden_labels
    ):
        raise BoundaryError("Trae pack gained capture/index Bazel authority")
    missing_labels = sorted(REQUIRED_BUILD_LABELS - build_labels)
    if missing_labels:
        raise BoundaryError(
            "Trae lower-layer Bazel inventory drifted: " + ", ".join(missing_labels)
        )
    required_target_lines = {
        'crate_name = "ctx_history_provider_trae"',
        'name = "test_support_lib"',
    }
    if not required_target_lines.issubset(build_lines):
        raise BoundaryError("Trae Bazel targets drifted")

    pack_text = _active_rust(_read_rust_tree(source_root), "Trae production source")
    _forbid_fragments(pack_text, FORBIDDEN_PACK_FRAGMENTS, "Trae pack")
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

    capture_facade_text = _active_rust(
        capture_facade.read_text(encoding="utf-8"), "capture Trae facade"
    )
    _require_fragments(
        capture_facade_text,
        (
            "pub(crate) use ctx_history_provider_trae::{",
            "trae_payload_admission",
            "TraePayloadAdmission",
            "TRAE_CHAT_KEYS",
            "TRAE_CHAT_ROWS_QUERY",
            "TRAE_SQLITE_VALUE_OVERHEAD_BYTES",
        ),
        "capture Trae facade",
    )
    _forbid_fragments(
        capture_facade_text,
        (
            "TraeReplacementTree",
            "CaptureProviderRuntime",
            "ProviderRuntimeBinding",
            "ReplacementDocumentTree",
        ),
        "capture Trae facade",
    )
    duplicate_root = capture_facade.with_suffix("")
    if duplicate_root.exists() and any(duplicate_root.rglob("*.rs")):
        raise BoundaryError("capture retained duplicate Trae production implementation")

    composition_data = tomllib.loads(composition_manifest.read_text(encoding="utf-8"))
    if composition_data.get("package", {}).get("name") != "ctx-history-capture-composition":
        raise BoundaryError("capture composition package identity drifted")
    composition_dependency = composition_data.get("dependencies", {}).get(
        "ctx-history-provider-trae"
    )
    if composition_dependency != {"path": "../ctx-history-provider-trae"}:
        raise BoundaryError("composition Trae Cargo dependency drifted")

    production_deps = _composition_dependencies(
        composition_build.read_text(encoding="utf-8")
    )
    trae_label = "//crates/ctx-history-provider-trae:lib"
    if production_deps.count(trae_label) != 1:
        raise BoundaryError(
            "composition production dependencies must contain exactly one " + trae_label
        )

    composition_facade_text = _active_rust(
        composition_facade.read_text(encoding="utf-8"), "composition Trae facade"
    )
    _require_fragments(
        composition_facade_text,
        (
            "pub(crate) type TraeReplacementTree",
            "ctx_history_provider_trae::TraeReplacementTree<",
            "crate::source_backed::family::CaptureProviderRuntime",
        ),
        "composition Trae facade",
    )

    registration_text = _active_rust(
        registration.read_text(encoding="utf-8"), "composition Trae route registration"
    )
    _require_fragments(
        registration_text,
        (
            "fn register_trae_route",
            "TraeReplacementTree::new(data_root, source.path.clone())",
            "register_replacement_document_tree_route_with_authority",
            "SourceBackedSelectorAuthority::DiscoveredWinner",
        ),
        "composition Trae route registration",
    )

    discovery_text = _active_rust(
        discovery.read_text(encoding="utf-8"), "capture Trae discovery probe"
    )
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
    parser.add_argument("capture_facade", type=Path)
    parser.add_argument("composition_manifest", type=Path)
    parser.add_argument("composition_build", type=Path)
    parser.add_argument("composition_facade", type=Path)
    parser.add_argument("registration", type=Path)
    parser.add_argument("discovery", type=Path)
    args = parser.parse_args(argv)
    try:
        validate(
            args.manifest,
            args.build,
            args.source_root,
            args.capture_facade,
            args.composition_manifest,
            args.composition_build,
            args.composition_facade,
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
