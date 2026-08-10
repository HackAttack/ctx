#!/usr/bin/env python3
"""Validate the static Cargo/Bazel and source-ownership semantic-index seam."""

from __future__ import annotations

import argparse
import re
import sys
import tomllib
from collections import Counter
from pathlib import Path
from typing import Any


EXPECTED_FEATURES = {"default": [], "test-support": []}
EXPECTED_DEPENDENCIES: dict[str, Any] = {
    "anyhow": {"workspace": True},
    "ctx-history-core": {"path": "../ctx-history-core"},
    "ctx-history-index": {"path": "../ctx-history-index"},
    "ctx-semantic-model": {"path": "../ctx-semantic-model"},
    "fs2": "0.4.3",
    "memmap2": {"workspace": True},
    "rusqlite": {"workspace": True},
    "serde": {"workspace": True},
    "serde_json": {"workspace": True},
    "sha2": {"workspace": True},
    "thiserror": {"workspace": True},
    "uuid": {"workspace": True},
}
EXPECTED_DEV_DEPENDENCIES: dict[str, Any] = {"tempfile": {"workspace": True}}
EXPECTED_INTERNAL_LABELS = [
    "//crates/ctx-history-core:lib",
    "//crates/ctx-history-index:lib",
    "//crates/ctx-semantic-model:lib",
]


class BoundaryError(RuntimeError):
    pass


def _extract_string_list(text: str, name: str) -> list[str]:
    match = re.search(rf"(?ms)^{re.escape(name)}\s*=\s*\[(.*?)^\]", text)
    if match is None:
        raise BoundaryError(f"missing Starlark list {name}")
    return re.findall(r'["\']([^"\']+)["\']', match.group(1))


def _assignment_values(text: str, name: str) -> Counter[str]:
    values = re.findall(rf"(?m)^\s*{re.escape(name)}\s*=\s*(.+),\s*$", text)
    return Counter(value.strip() for value in values)


def validate_manifest(path: Path) -> None:
    with path.open("rb") as handle:
        manifest = tomllib.load(handle)
    if manifest.get("features") != EXPECTED_FEATURES:
        raise BoundaryError(
            f"ctx-semantic-index Cargo features drifted: {manifest.get('features')!r}"
        )
    if manifest.get("dependencies") != EXPECTED_DEPENDENCIES:
        raise BoundaryError("ctx-semantic-index Cargo normal dependency inventory drifted")
    if manifest.get("dev-dependencies") != EXPECTED_DEV_DEPENDENCIES:
        raise BoundaryError("ctx-semantic-index Cargo dev dependency inventory drifted")


def validate_build(path: Path) -> None:
    text = re.sub(r"(?m)#.*$", "", path.read_text(encoding="utf-8"))
    if _extract_string_list(text, "CTX_SEMANTIC_INDEX_DEPS") != EXPECTED_INTERNAL_LABELS:
        raise BoundaryError("ctx-semantic-index Bazel internal dependency inventory drifted")

    expected_deps = Counter(
        {
            "all_crate_deps(normal = True) + CTX_SEMANTIC_INDEX_DEPS": 2,
            "all_crate_deps(normal = True, normal_dev = True) + CTX_SEMANTIC_INDEX_DEPS": 1,
        }
    )
    if _assignment_values(text, "deps") != expected_deps:
        raise BoundaryError("ctx-semantic-index Bazel dependency expressions drifted")

    expected_proc_macro_deps = Counter(
        {
            "all_crate_deps(proc_macro = True)": 2,
            "all_crate_deps(proc_macro = True, proc_macro_dev = True)": 1,
        }
    )
    if _assignment_values(text, "proc_macro_deps") != expected_proc_macro_deps:
        raise BoundaryError("ctx-semantic-index Bazel proc-macro dependency expressions drifted")

    expected_srcs = Counter(
        {
            'glob(["**"], exclude = ["BUILD.bazel"])': 1,
            "PROD_SRCS": 2,
            "RUST_SRCS": 1,
        }
    )
    if _assignment_values(text, "srcs") != expected_srcs:
        raise BoundaryError("ctx-semantic-index Bazel source target inventory drifted")

    expected_flags = Counter(
        {
            "CTX_SEMANTIC_INDEX_RUSTC_FLAGS": 2,
            "CTX_SEMANTIC_INDEX_RUSTC_FLAGS + ['--cfg=feature=\"test-support\"']": 1,
        }
    )
    if _assignment_values(text, "rustc_flags") != expected_flags:
        raise BoundaryError("ctx-semantic-index Bazel feature/cfg inventory drifted")


def validate_cli_partition(repo_root: Path) -> None:
    semantic_root = repo_root / "crates/ctx-cli/src/semantic"
    forbidden_exact = {
        "document.rs",
        "indexing.rs",
        "json.rs",
        "private_fs.rs",
        "query_index.rs",
        "tests/vector_store.rs",
        "vector_store.rs",
        "vector_store_schema.rs",
        "vector_store_search.rs",
        "vector_store_state.rs",
    }
    violations: list[str] = []
    for source in semantic_root.rglob("*.rs"):
        relative = source.relative_to(semantic_root).as_posix()
        if (
            relative in forbidden_exact
            or relative.startswith("query_index/")
            or relative.startswith("vector_store/")
        ):
            violations.append(relative)
    if violations:
        raise BoundaryError(
            "semantic-index-owned sources reappeared in ctx-cli: " + ", ".join(sorted(violations))
        )


def validate(repo_root: Path) -> None:
    validate_manifest(repo_root / "crates/ctx-semantic-index/Cargo.toml")
    validate_build(repo_root / "crates/ctx-semantic-index/BUILD.bazel")
    validate_cli_partition(repo_root)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("repo_root", type=Path)
    args = parser.parse_args()
    try:
        validate(args.repo_root.resolve())
    except (BoundaryError, OSError, tomllib.TOMLDecodeError) as error:
        print(f"semantic-index static boundary check failed: {error}", file=sys.stderr)
        return 1
    print("semantic-index static Cargo/Bazel/source boundary ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
