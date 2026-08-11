#!/usr/bin/env python3
"""Static dependency boundary for ctx-history-capture-runtime."""

from __future__ import annotations

import sys
import tomllib
from pathlib import Path
from typing import Any, Iterator


EXPECTED_DEPENDENCIES = {"uuid": {"workspace": True}}
EXPECTED_DEV_DEPENDENCIES = {"thiserror": {"workspace": True}}
RUNTIME_FORBIDDEN_CARGO = {
    "ctx-history-capture",
    "ctx-history-index",
    "ctx-history-jsonl",
}
RUNTIME_FORBIDDEN_BAZEL = (
    "//crates/ctx-history-capture:",
    "//crates/ctx-history-index:",
    "//crates/ctx-history-jsonl:",
    "provider",
)
JSONL_FORBIDDEN_CARGO = {"ctx-history-index", "ctx-history-index-format"}
JSONL_FORBIDDEN_BAZEL = (
    "//crates/ctx-history-index:",
    "//crates/ctx-history-index-format:",
    "@crates//:ctx-history-index",
    "@crates//:ctx-history-index-format",
)
DEPENDENCY_TABLE_NAMES = {"dependencies", "dev-dependencies", "build-dependencies"}


class BoundaryError(ValueError):
    pass


def _dependency_tables(
    manifest: dict[str, Any], package: str
) -> Iterator[tuple[str, dict[str, Any]]]:
    unexpected_top_level = sorted(
        name
        for name in manifest
        if name.endswith("dependencies") and name not in DEPENDENCY_TABLE_NAMES
    )
    if unexpected_top_level:
        raise BoundaryError(
            f"{package} Cargo has unsupported dependency tables: "
            + ", ".join(unexpected_top_level)
        )
    for table_name in DEPENDENCY_TABLE_NAMES:
        table = manifest.get(table_name, {})
        if not isinstance(table, dict):
            raise BoundaryError(
                f"{package} Cargo {table_name} table must be a table"
            )
        yield table_name, table

    target = manifest.get("target", {})
    if not isinstance(target, dict):
        raise BoundaryError(f"{package} Cargo target table must be a table")
    for target_name, target_tables in target.items():
        if not isinstance(target_tables, dict):
            raise BoundaryError(
                f"{package} Cargo target {target_name!r} table must be a table"
            )
        unexpected = sorted(set(target_tables) - DEPENDENCY_TABLE_NAMES)
        if unexpected:
            raise BoundaryError(
                f"{package} Cargo target {target_name!r} has unsupported tables: "
                + ", ".join(unexpected)
            )
        for table_name in DEPENDENCY_TABLE_NAMES:
            if table_name not in target_tables:
                continue
            table = target_tables[table_name]
            if not isinstance(table, dict):
                raise BoundaryError(
                    f"{package} Cargo target {target_name!r} {table_name} table "
                    "must be a table"
                )
            yield f"target.{target_name}.{table_name}", table


def _read_manifest(path: Path) -> dict[str, Any]:
    with path.open("rb") as handle:
        manifest = tomllib.load(handle)
    if not isinstance(manifest, dict):
        raise BoundaryError("Cargo manifest must be a table")
    return manifest


def _validate_no_forbidden_cargo_dependencies(
    manifest: dict[str, Any], package: str, forbidden: set[str]
) -> None:
    dependency_names = {
        name
        for _, table in _dependency_tables(manifest, package)
        for name in table
    }
    forbidden_dependencies = sorted(dependency_names & forbidden)
    if forbidden_dependencies:
        raise BoundaryError(
            f"{package} has forbidden Cargo dependencies: "
            + ", ".join(forbidden_dependencies)
        )


def _validate_no_forbidden_bazel_dependencies(
    build_path: Path, package: str, forbidden: tuple[str, ...]
) -> None:
    build_source = build_path.read_text(encoding="utf-8")
    forbidden_bazel = [label for label in forbidden if label in build_source]
    if forbidden_bazel:
        raise BoundaryError(
            f"{package} has forbidden Bazel dependencies: "
            + ", ".join(forbidden_bazel)
        )


def _validate_runtime_manifest(manifest_path: Path) -> None:
    package = "ctx-history-capture-runtime"
    manifest = _read_manifest(manifest_path)
    _validate_no_forbidden_cargo_dependencies(manifest, package, RUNTIME_FORBIDDEN_CARGO)
    dependencies = manifest.get("dependencies", {})
    if dependencies != EXPECTED_DEPENDENCIES:
        raise BoundaryError(
            "ctx-history-capture-runtime Cargo production dependencies drifted: "
            f"expected={sorted(EXPECTED_DEPENDENCIES)} actual={sorted(dependencies)}"
        )
    if manifest.get("dev-dependencies", {}) != EXPECTED_DEV_DEPENDENCIES:
        raise BoundaryError("ctx-history-capture-runtime Cargo dev dependencies drifted")
    build_dependencies = manifest.get("build-dependencies", {})
    if build_dependencies:
        raise BoundaryError(
            "ctx-history-capture-runtime Cargo build dependencies drifted: "
            f"expected=[] actual={sorted(build_dependencies)}"
        )
    target_dependency_tables = [
        table_name
        for table_name, _ in _dependency_tables(manifest, package)
        if table_name.startswith("target.")
    ]
    if target_dependency_tables:
        raise BoundaryError(
            "ctx-history-capture-runtime Cargo target-specific dependency-table "
            "bypass: "
            + ", ".join(target_dependency_tables)
        )


def validate(
    runtime_manifest_path: Path,
    runtime_build_path: Path,
    jsonl_manifest_path: Path,
    jsonl_build_path: Path,
) -> None:
    _validate_runtime_manifest(runtime_manifest_path)
    _validate_no_forbidden_bazel_dependencies(
        runtime_build_path,
        "ctx-history-capture-runtime",
        RUNTIME_FORBIDDEN_BAZEL,
    )
    jsonl_manifest = _read_manifest(jsonl_manifest_path)
    _validate_no_forbidden_cargo_dependencies(
        jsonl_manifest,
        "ctx-history-jsonl",
        JSONL_FORBIDDEN_CARGO,
    )
    _validate_no_forbidden_bazel_dependencies(
        jsonl_build_path,
        "ctx-history-jsonl",
        JSONL_FORBIDDEN_BAZEL,
    )


def main() -> int:
    if len(sys.argv) != 5:
        raise SystemExit(
            "usage: check_history_capture_runtime_boundary.py "
            "RUNTIME_CARGO RUNTIME_BUILD JSONL_CARGO JSONL_BUILD"
        )
    try:
        validate(
            Path(sys.argv[1]),
            Path(sys.argv[2]),
            Path(sys.argv[3]),
            Path(sys.argv[4]),
        )
    except (BoundaryError, OSError, tomllib.TOMLDecodeError) as error:
        print(error, file=sys.stderr)
        return 1
    print("ctx-history-capture-runtime/JSONL static dependency boundary ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
