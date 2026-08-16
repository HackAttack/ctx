#!/usr/bin/env python3
"""Validate live Cargo/Bazel ownership without a checked-in package inventory."""

from __future__ import annotations

from collections import defaultdict
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
import tomllib
from typing import Any

DEPENDENCY_TABLES = ("dependencies", "dev-dependencies", "build-dependencies")
TARGET_KINDS = ("bin", "test", "example", "bench")


class InventoryError(RuntimeError):
    pass


def fail(message: str) -> None:
    raise InventoryError(message)


def git(candidate: Path, *arguments: str) -> bytes:
    result = subprocess.run(
        ["git", "-C", os.fspath(candidate), *arguments],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        env={**os.environ, "GIT_OPTIONAL_LOCKS": "0", "LC_ALL": "C"},
    )
    if result.returncode != 0:
        fail(result.stderr.decode("utf-8", "replace").strip())
    return result.stdout


def repository_root() -> Path:
    for candidate in (Path.cwd(), Path(__file__).resolve().parents[2]):
        try:
            root = Path(git(candidate, "rev-parse", "--show-toplevel").decode().strip())
        except (InventoryError, UnicodeError):
            continue
        marker = root / ".git"
        if marker.is_symlink():
            resolved = marker.resolve()
            if resolved.name == ".git":
                root = resolved.parent
        root = root.resolve()
        try:
            verified = Path(
                git(root, "rev-parse", "--show-toplevel").decode().strip()
            ).resolve()
        except (InventoryError, UnicodeError):
            continue
        if verified == root:
            return root
    fail("could not locate the physical Git worktree")


def load_toml(path: Path) -> dict[str, Any]:
    try:
        return tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
        fail(f"cannot parse {path}: {error}")


def normalized_member(value: Any) -> str:
    if not isinstance(value, str) or not value:
        fail("workspace members must be nonempty strings")
    path = PurePosixPath(value)
    if path.is_absolute() or any(part in {"", ".", ".."} for part in path.parts):
        fail(f"workspace member is not normalized: {value!r}")
    if any(character in value for character in "*?[]\\"):
        fail(f"workspace member globs are unsupported; use an exact package path: {value}")
    return path.as_posix()


def tracked_package_manifests(root: Path) -> set[Path]:
    result: set[Path] = set()
    raw = git(root, "ls-files", "-z", "*/Cargo.toml", "*/*/Cargo.toml", "*/*/*/Cargo.toml")
    for item in raw.split(b"\0"):
        if not item:
            continue
        try:
            relative = Path(item.decode("utf-8"))
        except UnicodeDecodeError as error:
            raise InventoryError("Cargo manifest paths must be UTF-8") from error
        data = load_toml(root / relative)
        if "package" in data:
            result.add(relative)
    return result


def workspace_packages(root: Path) -> dict[str, tuple[Path, dict[str, Any]]]:
    workspace = load_toml(root / "Cargo.toml").get("workspace")
    if not isinstance(workspace, dict) or not isinstance(workspace.get("members"), list):
        fail("root Cargo.toml must define workspace.members")
    declared = {
        Path(member) / "Cargo.toml"
        for member in map(normalized_member, workspace["members"])
    }
    discovered = tracked_package_manifests(root)
    if declared != discovered:
        missing = sorted(path.as_posix() for path in discovered - declared)
        stale = sorted(path.as_posix() for path in declared - discovered)
        fail(f"workspace membership mismatch: missing={missing} stale={stale}")

    packages: dict[str, tuple[Path, dict[str, Any]]] = {}
    for relative in sorted(declared):
        manifest = root / relative
        if not manifest.is_file():
            fail(f"workspace manifest is missing: {relative}")
        data = load_toml(manifest)
        package = data.get("package")
        if not isinstance(package, dict) or not isinstance(package.get("name"), str):
            fail(f"workspace manifest has no package.name: {relative}")
        name = package["name"]
        if name in packages:
            fail(f"duplicate workspace package name: {name}")
        packages[name] = (manifest.parent, data)
    return packages


def explicit_targets(data: dict[str, Any], kind: str) -> list[dict[str, Any]]:
    value = data.get(kind, [])
    if not isinstance(value, list) or any(not isinstance(item, dict) for item in value):
        fail(f"Cargo [[{kind}]] targets must be tables")
    return value


def cargo_targets(package_dir: Path, data: dict[str, Any]) -> dict[str, Path]:
    package = data["package"]
    package_name = package["name"]
    targets: dict[str, Path] = {}
    lib = data.get("lib")
    if lib is not None and not isinstance(lib, dict):
        fail(f"{package_name} [lib] must be a table")
    if lib is not None or (package_dir / "src/lib.rs").is_file():
        target = lib or {}
        targets[f"lib:{target.get('name', package_name.replace('-', '_'))}"] = Path(
            target.get("path", "src/lib.rs")
        )

    defaults = {
        "bin": ("autobins", "src/main.rs", "src/bin", package_name),
        "test": ("autotests", None, "tests", None),
        "example": ("autoexamples", None, "examples", None),
        "bench": ("autobenches", None, "benches", None),
    }
    for kind in TARGET_KINDS:
        explicit = explicit_targets(data, kind)
        for item in explicit:
            name = item.get("name")
            if not isinstance(name, str) or not name:
                fail(f"{package_name} [[{kind}]] target has no name")
            default_path = f"{defaults[kind][2]}/{name}.rs"
            targets[f"{kind}:{name}"] = Path(item.get("path", default_path))
        if explicit or package.get(defaults[kind][0]) is False:
            continue
        flag, primary, directory, primary_name = defaults[kind]
        if primary and (package_dir / primary).is_file():
            targets[f"{kind}:{primary_name}"] = Path(primary)
        target_dir = package_dir / directory
        if target_dir.is_dir():
            for path in sorted(target_dir.glob("*.rs")):
                targets[f"{kind}:{path.stem}"] = path.relative_to(package_dir)

    build = package.get("build")
    if build is not False and (build or (package_dir / "build.rs").is_file()):
        targets["custom-build:build-script-build"] = Path(
            build if isinstance(build, str) else "build.rs"
        )
    return targets


def package_bazel_text(root: Path, package_dir: Path) -> str:
    files = [package_dir / "BUILD.bazel", *sorted(package_dir.glob("*.bzl"))]
    if not files[0].is_file():
        fail(f"Cargo package has no BUILD.bazel: {package_dir.relative_to(root)}")
    return "\n".join(path.read_text(encoding="utf-8") for path in files if path.is_file())


def assert_target_ownership(
    root: Path,
    package_name: str,
    package_dir: Path,
    data: dict[str, Any],
) -> int:
    text = package_bazel_text(root, package_dir)
    if re.search(
        r"\bname\s*=\s*[\"']cargo_package_data[\"']",
        text,
    ) is None:
        fail(f"{package_name} BUILD.bazel has no cargo_package_data target")
    targets = cargo_targets(package_dir, data)
    root_build = (root / "BUILD.bazel").read_text(encoding="utf-8")
    for target, relative in targets.items():
        path = relative.as_posix()
        if not (package_dir / relative).is_file():
            fail(f"{package_name} {target} source is missing: {path}")
        if path not in text and (
            target != "custom-build:build-script-build"
            or path not in root_build
        ):
            fail(f"{package_name} Cargo target is not owned by Bazel: {target} ({path})")
    return len(targets)


def dependency_entries(data: dict[str, Any]) -> list[tuple[str, str, Any]]:
    result: list[tuple[str, str, Any]] = []
    for table in DEPENDENCY_TABLES:
        value = data.get(table, {})
        if not isinstance(value, dict):
            fail(f"[{table}] must be a table")
        result.extend((table, name, entry) for name, entry in value.items())
    target = data.get("target", {})
    if not isinstance(target, dict):
        fail("[target] must be a table")
    for target_data in target.values():
        if not isinstance(target_data, dict):
            fail("target-specific dependency configuration must be a table")
        for table in DEPENDENCY_TABLES:
            value = target_data.get(table, {})
            if not isinstance(value, dict):
                fail(f"target-specific [{table}] must be a table")
            result.extend((table, name, entry) for name, entry in value.items())
    return result


def local_graph(
    root: Path,
    packages: dict[str, tuple[Path, dict[str, Any]]],
) -> dict[str, set[str]]:
    by_root = {directory.resolve(): name for name, (directory, _) in packages.items()}
    graph = {name: set() for name in packages}
    for name, (directory, data) in packages.items():
        bazel_text = package_bazel_text(root, directory)
        for table, dependency_name, value in dependency_entries(data):
            if not isinstance(value, dict) or "path" not in value:
                continue
            path = value["path"]
            if not isinstance(path, str):
                fail(f"{name} dependency {dependency_name} has a non-string path")
            resolved = (directory / path).resolve()
            target = by_root.get(resolved)
            if target is None:
                fail(f"{name} dependency {dependency_name} escapes the workspace: {path}")
            if table != "dev-dependencies":
                graph[name].add(target)
            package_label = f"//{resolved.relative_to(root).as_posix()}:"
            if package_label not in bazel_text and "all_crate_deps(" not in bazel_text:
                fail(f"{name} Bazel target omits Cargo path dependency {target}")
    return graph


def assert_acyclic(graph: dict[str, set[str]]) -> None:
    visiting: set[str] = set()
    visited: set[str] = set()

    def visit(package: str, stack: list[str]) -> None:
        if package in visiting:
            start = stack.index(package)
            fail(f"workspace dependency cycle: {' -> '.join(stack[start:] + [package])}")
        if package in visited:
            return
        visiting.add(package)
        stack.append(package)
        for dependency in sorted(graph[package]):
            visit(dependency, stack)
        stack.pop()
        visiting.remove(package)
        visited.add(package)

    for package in sorted(graph):
        visit(package, [])


def main() -> None:
    root = repository_root()
    packages = workspace_packages(root)
    target_count = sum(
        assert_target_ownership(root, name, directory, data)
        for name, (directory, data) in packages.items()
    )
    graph = local_graph(root, packages)
    assert_acyclic(graph)
    edge_count = sum(map(len, graph.values()))
    print(
        f"live Cargo/Bazel ownership covers {target_count} Cargo targets and "
        f"{edge_count} local edges across {len(packages)} discovered packages"
    )


if __name__ == "__main__":
    try:
        main()
    except (InventoryError, OSError) as error:
        print(f"rust target inventory check failed: {error}", file=sys.stderr)
        raise SystemExit(1) from None
