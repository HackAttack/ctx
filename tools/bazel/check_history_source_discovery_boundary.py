#!/usr/bin/env python3
"""Exact production dependency policy for ctx-history-source-discovery."""

from __future__ import annotations

import argparse
import sys
import tomllib
from pathlib import Path
from typing import Any, Iterable


EXPECTED_DEPENDENCIES: dict[str, Any] = {
    "chrono": {"workspace": True},
    "ctx-history-capture-model": {"path": "../ctx-history-capture-model"},
    "ctx-history-core": {"path": "../ctx-history-core"},
    "ctx-history-source-io": {"path": "../ctx-history-source-io"},
    "directories": {"workspace": True},
    "json5": {"workspace": True},
    "jsonc-parser": {"workspace": True},
    "libc": {"workspace": True},
    "quick-xml": {"workspace": True},
    "rusqlite": {"workspace": True},
    "serde": {"workspace": True},
    "serde_json": {"workspace": True},
    "serde_yaml": {"workspace": True},
    "sha2": {"workspace": True},
    "thiserror": {"workspace": True},
    "toml_edit": {"workspace": True},
}
EXPECTED_DEV_DEPENDENCIES: dict[str, Any] = {
    "ctx-history-source-io": {
        "path": "../ctx-history-source-io",
        "features": ["test-support"],
    },
    "tempfile": {"workspace": True},
}
EXPECTED_INTERNAL_CARGO = {
    "ctx-history-capture-model",
    "ctx-history-core",
    "ctx-history-source-io",
}
EXPECTED_INTERNAL_BAZEL = {
    "//crates/ctx-history-capture-model:lib",
    "//crates/ctx-history-core:lib",
    "//crates/ctx-history-source-discovery:lib",
    "//crates/ctx-history-source-io:lib",
}


class BoundaryError(RuntimeError):
    pass


def _describe_drift(expected: set[str], actual: set[str]) -> str:
    return f"missing={sorted(expected - actual)} extra={sorted(actual - expected)}"


def validate_manifest(path: Path) -> None:
    with path.open("rb") as handle:
        manifest = tomllib.load(handle)

    dependencies = manifest.get("dependencies", {})
    if dependencies != EXPECTED_DEPENDENCIES:
        raise BoundaryError(
            "ctx-history-source-discovery Cargo production dependency inventory drifted: "
            + _describe_drift(set(EXPECTED_DEPENDENCIES), set(dependencies))
        )
    internal = {name for name in dependencies if name.startswith("ctx-")}
    if internal != EXPECTED_INTERNAL_CARGO:
        raise BoundaryError(
            "ctx-history-source-discovery Cargo internal allowlist drifted: "
            + _describe_drift(EXPECTED_INTERNAL_CARGO, internal)
        )
    if manifest.get("dev-dependencies", {}) != EXPECTED_DEV_DEPENDENCIES:
        raise BoundaryError(
            "ctx-history-source-discovery Cargo dev dependency inventory drifted"
        )
    if "features" in manifest:
        raise BoundaryError(
            "ctx-history-source-discovery must not expose production feature switches"
        )

    dependency_bypasses = sorted(
        name
        for name in manifest
        if name.endswith("dependencies")
        and name not in {"dependencies", "dev-dependencies"}
    )
    if dependency_bypasses or "target" in manifest:
        raise BoundaryError(
            "ctx-history-source-discovery Cargo dependency-table bypass: "
            f"{dependency_bypasses or ['target']}"
        )


def validate_bazel_inventory(labels: Iterable[str], scope: str) -> None:
    actual = {label.strip() for label in labels if label.strip()}
    if actual != EXPECTED_INTERNAL_BAZEL:
        raise BoundaryError(
            f"ctx-history-source-discovery Bazel {scope} internal allowlist drifted: "
            + _describe_drift(EXPECTED_INTERNAL_BAZEL, actual)
        )


def validate(manifest: Path, direct_labels: Path, closure_labels: Path) -> None:
    validate_manifest(manifest)
    validate_bazel_inventory(direct_labels.read_text().splitlines(), "direct")
    validate_bazel_inventory(closure_labels.read_text().splitlines(), "transitive")


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("manifest", type=Path)
    parser.add_argument("direct_labels", type=Path)
    parser.add_argument("closure_labels", type=Path)
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    try:
        validate(args.manifest, args.direct_labels, args.closure_labels)
    except BoundaryError as error:
        print(error, file=sys.stderr)
        return 1
    print("ctx-history-source-discovery exact Cargo/Bazel dependency boundary ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
