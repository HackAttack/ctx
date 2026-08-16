#!/usr/bin/env python3
"""Exact dependency and production-ownership boundary for logical SQLite providers."""

from __future__ import annotations

import argparse
import re
import sys
import tomllib
from pathlib import Path
from typing import Any, Iterable


PACKAGE = "ctx-history-providers-sqlite-logical"
EXPECTED_DEPENDENCIES: dict[str, Any] = {
    "chrono": {"workspace": True},
    "ctx-history-capture-model": {"path": "../ctx-history-capture-model"},
    "ctx-history-capture-runtime": {"path": "../ctx-history-capture-runtime"},
    "ctx-history-core": {"path": "../ctx-history-core"},
    "ctx-history-source-io": {"path": "../ctx-history-source-io"},
    "ctx-history-source-sqlite": {"path": "../ctx-history-source-sqlite"},
    "rmpv": {"workspace": True},
    "rusqlite": {"workspace": True},
    "serde": {"workspace": True},
    "serde_json": {"workspace": True},
    "sha2": {"workspace": True},
    "thiserror": {"workspace": True},
    "uuid": {"workspace": True},
    "zstd": {"workspace": True},
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
    "//crates/ctx-history-providers-sqlite-logical:lib",
    "//crates/ctx-history-source-io:lib",
    "//crates/ctx-history-source-sqlite:lib",
}
EXPECTED_TRANSITIVE_BAZEL = EXPECTED_INTERNAL_BAZEL | {
    "//crates/ctx-history-platform:lib",
}
EXPECTED_PROVIDERS = {"deepagents", "forgecode", "opencode", "zed"}
ALLOWED_CAPTURE_PROVIDERS = {
    "DeepAgents",
    "ForgeCode",
    "Kilo",
    "MiMoCode",
    "OpenCode",
    "Zed",
}
FORBIDDEN_DEPENDENCIES = {
    "ctx-history-capture",
    "ctx-history-index",
    "ctx-history-index-format",
    "ctx-history-index-generation",
    "ctx-history-index-query",
    "ctx-history-jsonl",
    "ctx-history-source-discovery",
}


class BoundaryError(RuntimeError):
    pass


def _drift(expected: set[str], actual: set[str]) -> str:
    return f"missing={sorted(expected - actual)} extra={sorted(actual - expected)}"


def _load_manifest(path: Path) -> dict[str, Any]:
    with path.open("rb") as handle:
        value = tomllib.load(handle)
    if not isinstance(value, dict):
        raise BoundaryError(f"{path} must contain a TOML table")
    return value


def validate_manifest(path: Path) -> None:
    manifest = _load_manifest(path)
    package = manifest.get("package", {})
    if not isinstance(package, dict) or package.get("name") != PACKAGE:
        raise BoundaryError(f"logical SQLite provider package must be named {PACKAGE}")
    dependencies = manifest.get("dependencies", {})
    if dependencies != EXPECTED_DEPENDENCIES:
        raise BoundaryError(
            "logical SQLite provider production dependency inventory drifted: "
            + _drift(set(EXPECTED_DEPENDENCIES), set(dependencies))
        )
    dev_dependencies = manifest.get("dev-dependencies", {})
    if dev_dependencies != EXPECTED_DEV_DEPENDENCIES:
        raise BoundaryError("logical SQLite provider dev dependency inventory drifted")
    if set(dependencies) & FORBIDDEN_DEPENDENCIES or set(dev_dependencies) & FORBIDDEN_DEPENDENCIES:
        raise BoundaryError("logical SQLite provider gained a forbidden upward dependency")
    bypasses = sorted(
        name
        for name in manifest
        if name.endswith("dependencies")
        and name not in {"dependencies", "dev-dependencies"}
    )
    if bypasses or "target" in manifest or "features" in manifest:
        raise BoundaryError("logical SQLite provider dependency-table or feature bypass")


def validate_composition_manifest(path: Path) -> None:
    dependencies = _load_manifest(path).get("dependencies", {})
    expected = {"path": "../ctx-history-providers-sqlite-logical"}
    if dependencies.get(PACKAGE) != expected:
        raise BoundaryError("composition must depend on the logical SQLite provider pack")


def validate_bazel_inventory(labels: Iterable[str], scope: str) -> None:
    actual = {label.strip() for label in labels if label.strip()}
    expected = EXPECTED_TRANSITIVE_BAZEL if scope == "transitive" else EXPECTED_INTERNAL_BAZEL
    if actual != expected:
        raise BoundaryError(
            f"logical SQLite provider Bazel {scope} allowlist drifted: "
            + _drift(expected, actual)
        )


def _rust_files(root: Path) -> list[Path]:
    return sorted(path for path in root.rglob("*.rs") if path.is_file())


def validate_pack_sources(root: Path) -> None:
    providers = root / "providers"
    top_level = {path.stem for path in providers.glob("*.rs")}
    directories = {path.name for path in providers.iterdir() if path.is_dir()}
    if top_level != EXPECTED_PROVIDERS or directories != EXPECTED_PROVIDERS:
        raise BoundaryError(
            "logical SQLite provider ownership set drifted: "
            + _drift(EXPECTED_PROVIDERS, top_level | directories)
        )
    registration = (root / "registration.rs").read_text(encoding="utf-8")
    registered = set(re.findall(r"CaptureProvider::([A-Za-z0-9_]+)", registration))
    if registered != ALLOWED_CAPTURE_PROVIDERS:
        raise BoundaryError(
            "logical SQLite registration provider set drifted: "
            + _drift(ALLOWED_CAPTURE_PROVIDERS, registered)
        )
    production = "\n".join(path.read_text(encoding="utf-8") for path in _rust_files(root))
    forbidden = {
        token
        for token in (
            "ctx_history_capture::",
            "ctx_history_index::",
            "ctx_history_index_format::",
            "ctx_history_index_generation::",
            "ctx_history_index_query::",
            "ctx_history_jsonl::",
            "ctx_history_source_discovery::",
        )
        if token in production
    }
    if forbidden:
        raise BoundaryError(
            "logical SQLite provider source contains forbidden authority imports: "
            + ", ".join(sorted(forbidden))
        )
    if re.search(r"(?i)\b(?:hermes|trae)\b", production):
        raise BoundaryError("logical SQLite provider source absorbed Hermes or Trae")


def validate_composition_sources(root: Path) -> None:
    logical = (
        root
        / "source_backed/registration/families/sqlite/logical.rs"
    ).read_text(encoding="utf-8")
    if "ctx_history_providers_sqlite_logical" not in logical:
        raise BoundaryError("composition logical SQLite registration does not consume the provider pack")


def validate_neutral_source(root: Path, label: str) -> None:
    for path in _rust_files(root):
        if "ctx_history_providers_sqlite_logical" in path.read_text(encoding="utf-8"):
            raise BoundaryError(f"{label} assembles the concrete logical SQLite provider pack")


def validate(
    manifest: Path,
    source_root: Path,
    composition_manifest: Path,
    composition_source_root: Path,
    runtime_source_root: Path,
    discovery_source_root: Path,
    direct_labels: Path,
    closure_labels: Path,
) -> None:
    validate_manifest(manifest)
    validate_composition_manifest(composition_manifest)
    validate_bazel_inventory(direct_labels.read_text().splitlines(), "direct")
    validate_bazel_inventory(closure_labels.read_text().splitlines(), "transitive")
    validate_pack_sources(source_root)
    validate_composition_sources(composition_source_root)
    validate_neutral_source(runtime_source_root, "capture runtime")
    validate_neutral_source(discovery_source_root, "source discovery")


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("manifest", type=Path)
    parser.add_argument("source_root", type=Path)
    parser.add_argument("capture_manifest", type=Path)
    parser.add_argument("capture_source_root", type=Path)
    parser.add_argument("runtime_source_root", type=Path)
    parser.add_argument("discovery_source_root", type=Path)
    parser.add_argument("direct_labels", type=Path)
    parser.add_argument("closure_labels", type=Path)
    args = parser.parse_args(argv)
    try:
        validate(
            args.manifest,
            args.source_root,
            args.capture_manifest,
            args.capture_source_root,
            args.runtime_source_root,
            args.discovery_source_root,
            args.direct_labels,
            args.closure_labels,
        )
    except (BoundaryError, OSError, tomllib.TOMLDecodeError) as error:
        print(error, file=sys.stderr)
        return 1
    print("logical SQLite provider dependency and production-ownership boundary ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
