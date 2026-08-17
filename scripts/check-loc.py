#!/usr/bin/env python3
"""Enforce fixed repository-wide physical line limits without an allowlist."""

from __future__ import annotations

import argparse
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys

SOURCE_LIMIT = 1_000
TEST_LIMIT = 1_500
SOURCE_EXTENSIONS = {
    ".bash", ".bzl", ".c", ".cc", ".cjs", ".cpp", ".cs", ".cxx", ".go",
    ".h", ".hh", ".hpp", ".hxx", ".java", ".js", ".jsx", ".kt", ".kts",
    ".mjs", ".ps1", ".psm1", ".py", ".rs", ".sh", ".swift", ".ts", ".tsx",
}
BAZEL_DECLARATIONS = {
    "BUILD", "BUILD.bazel", "MODULE.bazel", "WORKSPACE", "WORKSPACE.bazel",
}
TEST_COMPONENTS = {"Tests", "__tests__", "benches", "test", "test_support", "tests"}
DOC_NAMES = {"LICENSE", "NOTICE", "README", "SECURITY.md"}
DOC_SUFFIXES = {
    ".json", ".jsonl", ".lock", ".markdown", ".md", ".rst", ".toml", ".txt",
    ".yaml", ".yml",
}


class CheckError(RuntimeError):
    pass


def git(candidate: Path, *arguments: str) -> bytes:
    result = subprocess.run(
        ["git", "-C", os.fspath(candidate), *arguments],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        env={**os.environ, "GIT_OPTIONAL_LOCKS": "0", "LC_ALL": "C"},
    )
    if result.returncode != 0:
        detail = result.stderr.decode("utf-8", "replace").strip()
        raise CheckError(f"git {' '.join(arguments)} failed: {detail}")
    return result.stdout


def validated_root(candidate: Path) -> Path | None:
    try:
        root = Path(git(candidate, "rev-parse", "--show-toplevel").decode().strip())
    except (CheckError, UnicodeError):
        return None
    marker = root / ".git"
    if marker.is_symlink():
        marker = marker.resolve()
        root = marker.parent if marker.name == ".git" else root
    try:
        physical = root.resolve(strict=True)
        verified = Path(
            git(physical, "rev-parse", "--show-toplevel").decode().strip()
        ).resolve(strict=True)
    except (CheckError, OSError, UnicodeError):
        return None
    return physical if verified == physical else None


def repository_root(explicit: Path | None) -> Path:
    candidates = [
        explicit,
        Path(value) if (value := os.environ.get("CTX_LOC_REPO_ROOT")) else None,
        Path(value) if (value := os.environ.get("BUILD_WORKSPACE_DIRECTORY")) else None,
        Path.cwd(),
        Path(__file__).resolve().parent.parent,
    ]
    for candidate in candidates:
        if candidate is not None and (root := validated_root(candidate)) is not None:
            return root
    raise CheckError("could not locate the physical Git worktree")


def normalized_path(raw: bytes) -> str:
    try:
        value = raw.decode("utf-8")
    except UnicodeDecodeError as error:
        raise CheckError("repository paths must be UTF-8") from error
    if any(character in value for character in ("\x00", "\t", "\n", "\r", "\\")):
        raise CheckError(f"unsupported repository path: {value!r}")
    path = PurePosixPath(value)
    if path.is_absolute() or any(part in {"", ".", ".."} for part in path.parts):
        raise CheckError(f"non-normalized repository path: {value!r}")
    return value


def classify(path: str) -> str | None:
    pure = PurePosixPath(path)
    base = pure.name
    parents = pure.parts[:-1]
    if base in BAZEL_DECLARATIONS:
        return None
    if parents and parents[0] in {"docs", "fixture", "fixtures"}:
        return None
    if any(
        part in {"fixture", "fixtures"}
        and any(prefix in TEST_COMPONENTS for prefix in parents[:index])
        for index, part in enumerate(parents)
    ):
        return None
    if base in {"Cargo.lock", "MODULE.bazel.lock", "package-lock.json"}:
        return None
    if base.endswith(".lock"):
        return None
    if base in DOC_NAMES:
        return None
    suffix = pure.suffix.lower()
    if suffix in DOC_SUFFIXES or suffix not in SOURCE_EXTENSIONS:
        return None

    is_test = any(part in TEST_COMPONENTS for part in parents)
    is_test = is_test or base == "tests.rs" or base.startswith("test_support")
    is_test = is_test or re.search(r"_(?:test|tests)\.[^.]+$", base) is not None
    is_test = is_test or re.search(
        r"\.(?:test|spec)\.(?:js|jsx|mjs|cjs|ts|tsx)$", base
    ) is not None
    is_test = is_test or base.endswith("Tests.swift")
    return "test" if is_test else "source"


def crosses_symlink(root: Path, path: str) -> bool:
    current = root
    for component in PurePosixPath(path).parts:
        current /= component
        if current.is_symlink():
            return True
    return False


def physical_lines(path: Path) -> int:
    content = path.read_bytes()
    return content.count(b"\n") + int(bool(content) and not content.endswith(b"\n"))


def inventory(root: Path) -> list[str]:
    raw = git(root, "ls-files", "-z", "--cached", "--others", "--exclude-standard")
    return sorted({normalized_path(item) for item in raw.split(b"\0") if item})


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path)
    arguments = parser.parse_args(argv)
    root = repository_root(arguments.root)

    violations: list[tuple[int, str, str, int, int]] = []
    counted = 0
    for relative in inventory(root):
        kind = classify(relative)
        if kind is None:
            continue
        path = root / relative
        if not path.exists() and not path.is_symlink():
            continue
        if crosses_symlink(root, relative):
            raise CheckError(f"refusing to follow source path through a symlink: {relative}")
        if not path.is_file():
            continue
        counted += 1
        lines = physical_lines(path)
        limit = TEST_LIMIT if kind == "test" else SOURCE_LIMIT
        if lines > limit:
            violations.append((lines - limit, relative, kind, lines, limit))

    if violations:
        print(
            "LOC gate failed; hard limits are source=1000 physical lines and "
            "test=1500 physical lines.",
            file=sys.stderr,
        )
        print("Largest excess first:", file=sys.stderr)
        for excess, path, kind, lines, limit in sorted(
            violations, key=lambda item: (-item[0], item[1])
        ):
            print(
                f"  {path} ({kind}): {lines} lines > limit {limit} (+{excess})",
                file=sys.stderr,
            )
        return 1
    print(
        f"LOC gate passed ({counted} live Git source files; source <= 1000, "
        "test <= 1500; Bazel declarations excluded; no exceptions)."
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (CheckError, OSError) as error:
        print(f"loc gate failed: {error}", file=sys.stderr)
        raise SystemExit(1) from None
