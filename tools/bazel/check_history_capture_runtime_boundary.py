#!/usr/bin/env python3
"""Static dependency boundary for ctx-history-capture-runtime."""

from __future__ import annotations

import sys
import tomllib
from pathlib import Path


EXPECTED_DEPENDENCIES = {"uuid": {"workspace": True}}
EXPECTED_DEV_DEPENDENCIES = {"thiserror": {"workspace": True}}
FORBIDDEN_CARGO = {
    "ctx-history-capture",
    "ctx-history-index",
    "ctx-history-jsonl",
}
FORBIDDEN_BAZEL = (
    "//crates/ctx-history-capture:",
    "//crates/ctx-history-index:",
    "//crates/ctx-history-jsonl:",
    "provider",
)


def validate(manifest_path: Path, build_path: Path) -> None:
    with manifest_path.open("rb") as handle:
        manifest = tomllib.load(handle)
    dependencies = manifest.get("dependencies", {})
    dependency_names = {
        name
        for table, value in manifest.items()
        if table.endswith("dependencies") and isinstance(value, dict)
        for name in value
    }
    forbidden_cargo = sorted(dependency_names & FORBIDDEN_CARGO)
    if forbidden_cargo:
        raise ValueError(
            "ctx-history-capture-runtime has forbidden Cargo dependencies: "
            + ", ".join(forbidden_cargo)
        )
    if dependencies != EXPECTED_DEPENDENCIES:
        raise ValueError(
            "ctx-history-capture-runtime production dependencies drifted: "
            f"expected={sorted(EXPECTED_DEPENDENCIES)} actual={sorted(dependencies)}"
        )
    if manifest.get("dev-dependencies", {}) != EXPECTED_DEV_DEPENDENCIES:
        raise ValueError("ctx-history-capture-runtime dev dependencies drifted")
    build_source = build_path.read_text(encoding="utf-8")
    forbidden_bazel = [label for label in FORBIDDEN_BAZEL if label in build_source]
    if forbidden_bazel:
        raise ValueError(
            "ctx-history-capture-runtime has forbidden Bazel dependencies: "
            + ", ".join(forbidden_bazel)
        )


def main() -> int:
    if len(sys.argv) != 3:
        raise SystemExit("usage: check_history_capture_runtime_boundary.py CARGO BUILD")
    try:
        validate(Path(sys.argv[1]), Path(sys.argv[2]))
    except ValueError as error:
        print(error, file=sys.stderr)
        return 1
    print("ctx-history-capture-runtime static dependency boundary ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
