#!/usr/bin/env python3
"""Enforce a physical 20,000-CLOC limit for Cargo workspace packages."""

from __future__ import annotations

from dataclasses import dataclass
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
import tempfile
import tomli as tomllib
from typing import Any, Iterable


HARD_LIMIT = 20_000
METRIC = "physical-rust-cloc-v1"
POLICY_PATH = "scripts/check-rust-crate-size-policy-v1.json"
COMMIT = re.compile(r"^[0-9a-f]{40}$")
EXCLUDED_DIRECTORY_NAMES = {
    ".git",
    ".hg",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".svn",
    "__pycache__",
    "node_modules",
    "target",
}
UPDATE_COMMAND = (
    'scripts/bazelw run //:rust_crate_size_preflight -- --update-ratchets "$PWD"'
)


class GateError(RuntimeError):
    pass


@dataclass(frozen=True)
class Package:
    name: str
    manifest: str
    root: str


@dataclass(frozen=True)
class Measurement:
    package: Package
    cloc: int
    files: int


TEMPORARY_OWNERSHIP_TRANSITION = {
    "ctx": ("crates/ctx-cli/Cargo.toml", 82_737, 69_506),
    "ctx-history-capture": ("crates/ctx-history-capture/Cargo.toml", 165_225, 172_360),
}


def normalized_relative_path(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise GateError(f"{label} must be a nonempty relative path")
    if any(character in value for character in ("\0", "\t", "\n", "\r", "\\")):
        raise GateError(f"{label} is not normalized: {value!r}")
    if any(character in value for character in "*?["):
        raise GateError(f"{label} may not contain a glob: {value}")
    path = PurePosixPath(value)
    if path.is_absolute() or value.endswith("/") or any(part in {"", ".", ".."} for part in path.parts):
        raise GateError(f"{label} is not normalized: {value}")
    return value


def read_toml(path: Path, label: str) -> dict[str, Any]:
    try:
        value = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
        raise GateError(f"{label} is not valid UTF-8 TOML: {error}") from error
    if not isinstance(value, dict):
        raise GateError(f"{label} root must be a table")
    return value


def read_json(path: Path, label: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise GateError(f"{label} is not valid UTF-8 JSON: {error}") from error
    if not isinstance(value, dict):
        raise GateError(f"{label} root must be an object")
    return value


def has_symlink_component(root: Path, relative: str) -> bool:
    current = root
    for part in PurePosixPath(relative).parts:
        current = current / part
        if current.is_symlink():
            return True
    return False


def workspace_packages(root: Path) -> list[Package]:
    manifest = root / "Cargo.toml"
    if not manifest.is_file() or manifest.is_symlink():
        raise GateError("root Cargo.toml must be a regular non-symlink file")
    workspace = read_toml(manifest, "root Cargo.toml").get("workspace")
    if not isinstance(workspace, dict) or not isinstance(workspace.get("members"), list):
        raise GateError("root Cargo.toml must declare workspace.members")
    members = workspace["members"]
    if not members:
        raise GateError("workspace.members must not be empty")

    packages: list[Package] = []
    seen_roots: set[str] = set()
    seen_names: set[str] = set()
    for raw_member in members:
        member = normalized_relative_path(raw_member, "workspace member")
        if member in seen_roots:
            raise GateError(f"duplicate workspace member: {member}")
        if has_symlink_component(root, member):
            raise GateError(f"workspace package root contains a symlink component: {member}")
        package_root = root / member
        package_manifest = package_root / "Cargo.toml"
        if not package_root.is_dir() or not package_manifest.is_file() or package_manifest.is_symlink():
            raise GateError(f"workspace member has no regular Cargo.toml: {member}")
        package_table = read_toml(package_manifest, f"{member}/Cargo.toml").get("package")
        name = package_table.get("name") if isinstance(package_table, dict) else None
        if not isinstance(name, str) or not name:
            raise GateError(f"workspace member package.name is malformed: {member}")
        if name in seen_names:
            raise GateError(f"duplicate workspace package name: {name}")
        seen_roots.add(member)
        seen_names.add(name)
        packages.append(Package(name=name, manifest=f"{member}/Cargo.toml", root=member))

    roots = sorted(seen_roots)
    for index, package_root in enumerate(roots):
        nested = [other for other in roots[index + 1 :] if other.startswith(package_root + "/")]
        if nested:
            raise GateError(f"overlapping or nested workspace package roots: {package_root}, {nested[0]}")
    return sorted(packages, key=lambda package: package.name)


def checkout_artifact_directory(relative: PurePosixPath) -> bool:
    name = relative.name
    if name in EXCLUDED_DIRECTORY_NAMES:
        return True
    return len(relative.parts) == 1 and (name.startswith("bazel-") or name == ".buildkite-cache")


def beneath_package(relative: PurePosixPath, package_roots: tuple[str, ...]) -> bool:
    path = relative.as_posix()
    return any(path == package_root or path.startswith(package_root + "/") for package_root in package_roots)


def physical_repository_files(
    root: Path, packages: list[Package]
) -> tuple[set[str], set[str]]:
    rust_files: set[str] = set()
    manifests: set[str] = set()
    package_roots = tuple(package.root for package in packages)

    def visit(directory: Path, relative: PurePosixPath) -> None:
        try:
            entries = sorted(os.scandir(directory), key=lambda entry: entry.name)
        except OSError as error:
            raise GateError(f"cannot scan repository directory {relative.as_posix()}: {error}") from error
        for entry in entries:
            child_relative = relative / entry.name
            child_path = Path(entry.path)
            package_owned = beneath_package(child_relative, package_roots)
            if entry.is_symlink():
                if entry.name.endswith(".rs"):
                    raise GateError(f"symlinked Rust file is forbidden: {child_relative.as_posix()}")
                if entry.name == "Cargo.toml":
                    raise GateError(f"symlinked Cargo.toml is forbidden: {child_relative.as_posix()}")
                if package_owned and entry.is_dir(follow_symlinks=True):
                    raise GateError(
                        f"symlinked package directory is forbidden: {child_relative.as_posix()}"
                    )
                if (
                    not package_owned
                    and entry.is_dir(follow_symlinks=True)
                    and not checkout_artifact_directory(child_relative)
                ):
                    raise GateError(
                        f"symlinked repository directory is ambiguous: {child_relative.as_posix()}"
                    )
                continue
            if entry.is_dir(follow_symlinks=False):
                if package_owned or not checkout_artifact_directory(child_relative):
                    visit(child_path, child_relative)
                continue
            if entry.name.endswith(".rs"):
                if not entry.is_file(follow_symlinks=False):
                    raise GateError(f"Rust path is not a regular file: {child_relative.as_posix()}")
                rust_files.add(child_relative.as_posix())
            if entry.name == "Cargo.toml":
                if not entry.is_file(follow_symlinks=False):
                    raise GateError(f"Cargo.toml is not a regular file: {child_relative.as_posix()}")
                manifests.add(child_relative.as_posix())

    visit(root, PurePosixPath())
    return rust_files, manifests


def assign_physical_sources(
    packages: list[Package], rust_files: Iterable[str], manifests: set[str]
) -> dict[str, list[str]]:
    expected_manifests = {"Cargo.toml", *(package.manifest for package in packages)}
    if manifests != expected_manifests:
        raise GateError(
            "undeclared Cargo.toml detected: "
            f"extra={sorted(manifests-expected_manifests)}, missing={sorted(expected_manifests-manifests)}"
        )
    result = {package.name: [] for package in packages}
    for path in sorted(rust_files):
        owners = [package for package in packages if path.startswith(package.root + "/")]
        if len(owners) != 1:
            raise GateError(
                f"Rust file must belong physically to exactly one workspace package: {path}; "
                f"owners={[package.name for package in owners]}"
            )
        result[owners[0].name].append(path)
    for package in packages:
        if not result[package.name]:
            raise GateError(f"workspace package has no physical Rust files: {package.name}")
    return result


def raw_string_start(line: str, index: int) -> tuple[int, int] | None:
    if index and (line[index - 1].isalnum() or line[index - 1] == "_"):
        return None
    cursor = index
    if line.startswith(("br", "cr"), index):
        cursor += 2
    elif line.startswith("r", index):
        cursor += 1
    else:
        return None
    hashes = 0
    while cursor < len(line) and line[cursor] == "#":
        hashes += 1
        cursor += 1
    if cursor < len(line) and line[cursor] == '"':
        return hashes, cursor + 1
    return None


def rust_character_end(line: str, index: int) -> int | None:
    """Return the byte-character/character literal end, not a lifetime tick."""
    cursor = index + 1
    if cursor >= len(line) or line[cursor] in "\r\n'":
        return None
    if line[cursor] == "\\":
        cursor += 1
        if cursor >= len(line) or line[cursor] in "\r\n":
            return None
        if line[cursor] == "x":
            cursor += 3
        elif line[cursor] == "u" and cursor + 1 < len(line) and line[cursor + 1] == "{":
            closing = line.find("}", cursor + 2)
            if closing < 0:
                return None
            cursor = closing + 1
        else:
            cursor += 1
    else:
        cursor += 1
    if cursor < len(line) and line[cursor] == "'":
        return cursor + 1
    return None


def rust_cloc(content: bytes, path: str = "Rust source") -> int:
    try:
        text = content.decode("utf-8")
    except UnicodeDecodeError as error:
        raise GateError(f"{path} is not UTF-8") from error
    block_depth = 0
    string_kind: str | None = None
    raw_hashes = 0
    escaped = False
    count = 0
    for line in text.splitlines(keepends=True):
        code = string_kind is not None
        index = 0
        while index < len(line):
            if block_depth:
                if line.startswith("/*", index):
                    block_depth += 1
                    index += 2
                elif line.startswith("*/", index):
                    block_depth -= 1
                    index += 2
                else:
                    index += 1
                continue
            if string_kind == "raw":
                code = True
                terminator = '"' + ("#" * raw_hashes)
                if line.startswith(terminator, index):
                    string_kind = None
                    index += len(terminator)
                else:
                    index += 1
                continue
            if string_kind == "quoted":
                code = True
                character = line[index]
                if escaped:
                    escaped = False
                elif character == "\\":
                    escaped = True
                elif character == '"':
                    string_kind = None
                index += 1
                continue

            if line[index].isspace():
                index += 1
                continue
            if line.startswith("//", index):
                break
            if line.startswith("/*", index):
                block_depth = 1
                index += 2
                continue
            raw = raw_string_start(line, index)
            if raw is not None:
                raw_hashes, index = raw
                string_kind = "raw"
                code = True
                continue
            if line.startswith(('b"', 'c"'), index):
                string_kind = "quoted"
                escaped = False
                code = True
                index += 2
                continue
            character_index = index + 1 if line.startswith("b'", index) else index
            if line[character_index] == "'":
                character_end = rust_character_end(line, character_index)
                if character_end is not None:
                    code = True
                    index = character_end
                    continue
            if line[index] == '"':
                string_kind = "quoted"
                escaped = False
                code = True
                index += 1
                continue
            code = True
            index += 1
        if code:
            count += 1
    if block_depth:
        raise GateError(f"{path} has an unterminated block comment")
    if string_kind is not None:
        raise GateError(f"{path} has an unterminated string literal")
    return count


def measure_packages(root: Path, packages: list[Package], sources: dict[str, list[str]]) -> list[Measurement]:
    result: list[Measurement] = []
    for package in packages:
        paths = sources[package.name]
        cloc = 0
        for path in paths:
            try:
                content = (root / path).read_bytes()
            except OSError as error:
                raise GateError(f"cannot read Rust source {path}: {error}") from error
            cloc += rust_cloc(content, path)
        result.append(Measurement(package=package, cloc=cloc, files=len(paths)))
    return result


def live_measurements(root: Path) -> list[Measurement]:
    packages = workspace_packages(root)
    rust_files, manifests = physical_repository_files(root, packages)
    sources = assign_physical_sources(packages, rust_files, manifests)
    return measure_packages(root, packages, sources)


def parse_policy(value: dict[str, Any], label: str) -> dict[str, dict[str, Any]]:
    expected = {"schema_version", "metric", "hard_limit", "admission_sha", "offenders"}
    if set(value) != expected or value.get("schema_version") != 1:
        raise GateError(f"{label} policy schema is unsupported")
    if value.get("metric") != METRIC or value.get("hard_limit") != HARD_LIMIT:
        raise GateError(f"{label} policy must use {METRIC} and one hard limit of {HARD_LIMIT}")
    admission = value.get("admission_sha")
    if not isinstance(admission, str) or COMMIT.fullmatch(admission) is None:
        raise GateError(f"{label} admission_sha is malformed")
    entries = value.get("offenders")
    if not isinstance(entries, list):
        raise GateError(f"{label} offenders must be an array")
    names: list[str] = []
    result: dict[str, dict[str, Any]] = {}
    for entry in entries:
        if not isinstance(entry, dict) or set(entry) != {"package", "manifest", "ratchet"}:
            raise GateError(f"{label} offender entry is malformed")
        name = entry.get("package")
        manifest = entry.get("manifest")
        ratchet = entry.get("ratchet")
        if not isinstance(name, str) or not name:
            raise GateError(f"{label} offender package is malformed")
        normalized_relative_path(manifest, f"{label} manifest for {name}")
        if (
            not isinstance(ratchet, int)
            or isinstance(ratchet, bool)
            or ratchet <= HARD_LIMIT
        ):
            raise GateError(f"{label} ratchet is malformed: {name}")
        names.append(name)
        result[name] = entry
    if names != sorted(names) or len(names) != len(set(names)):
        raise GateError(f"{label} offenders must be sorted and unique")
    return result


def canonical_json(value: Any) -> bytes:
    return (json.dumps(value, indent=2, ensure_ascii=False) + "\n").encode()


def isolated_git(root: Path, *arguments: str, check: bool = True) -> subprocess.CompletedProcess[bytes]:
    environment = {
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_OPTIONAL_LOCKS": "0",
        "LC_ALL": "C",
        "PATH": os.environ.get("PATH", os.defpath),
    }
    result = subprocess.run(
        ["git", "-c", f"core.excludesFile={os.devnull}", *arguments],
        cwd=root,
        env=environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if check and result.returncode != 0:
        raise GateError(
            f"git {' '.join(arguments)} failed: {result.stderr.decode('utf-8', 'replace').strip()}"
        )
    return result


def previous_accepted_policy(root: Path) -> tuple[str, dict[str, Any] | None]:
    top = Path(isolated_git(root, "rev-parse", "--show-toplevel").stdout.decode().strip()).resolve()
    if top != root.resolve():
        raise GateError(f"preflight requires the Git checkout root: {root}")
    head = isolated_git(root, "rev-parse", "HEAD").stdout.decode().strip()
    origin = isolated_git(root, "rev-parse", "refs/remotes/origin/main").stdout.decode().strip()
    local_main_result = isolated_git(root, "rev-parse", "refs/heads/main", check=False)
    if local_main_result.returncode == 0:
        local_main = local_main_result.stdout.decode().strip()
        origin_precedes_local_main = isolated_git(
            root, "merge-base", "--is-ancestor", origin, local_main, check=False
        ).returncode == 0
        if local_main != origin and origin_precedes_local_main:
            raise GateError(
                f"origin/main is stale relative to local main: origin={origin}, local_main={local_main}"
            )
    if head == origin:
        base = isolated_git(root, "rev-parse", "HEAD^1").stdout.decode().strip()
    else:
        base = isolated_git(root, "merge-base", "HEAD", "refs/remotes/origin/main").stdout.decode().strip()
        if base != origin:
            raise GateError(f"origin/main advanced beyond candidate base: origin={origin}, merge_base={base}")
    shown = isolated_git(root, "show", f"{base}:{POLICY_PATH}", check=False)
    if shown.returncode != 0:
        return base, None
    try:
        value = json.loads(shown.stdout)
    except (UnicodeError, json.JSONDecodeError) as error:
        raise GateError(f"previous accepted policy is malformed: {error}") from error
    if not isinstance(value, dict):
        raise GateError("previous accepted policy root must be an object")
    return base, value


def is_temporary_ownership_transition(
    entries: dict[str, dict[str, Any]],
    previous_entries: dict[str, dict[str, Any]],
    measured: dict[str, Measurement],
) -> bool:
    old_entries = {
        name: {"package": name, "manifest": manifest, "ratchet": old}
        for name, (manifest, old, _new) in TEMPORARY_OWNERSHIP_TRANSITION.items()
    }
    new_entries = {
        name: {"package": name, "manifest": manifest, "ratchet": new}
        for name, (manifest, _old, new) in TEMPORARY_OWNERSHIP_TRANSITION.items()
    }
    return previous_entries == old_entries and entries == new_entries and all(
        name in measured
        and measured[name].package.manifest == manifest
        and measured[name].cloc == new
        for name, (manifest, _old, new) in TEMPORARY_OWNERSHIP_TRANSITION.items()
    )


def validate_policy_transition(
    candidate: dict[str, Any],
    previous: dict[str, Any] | None,
    base_sha: str,
    measurements: list[Measurement],
) -> dict[str, dict[str, Any]]:
    entries = parse_policy(candidate, "candidate")
    measured = {item.package.name: item for item in measurements}
    if previous is None:
        if candidate["admission_sha"] != base_sha:
            raise GateError(
                "bootstrap admission_sha must equal accepted base: "
                f"policy={candidate['admission_sha']} base={base_sha}"
            )
        offenders = {name for name, item in measured.items() if item.cloc > HARD_LIMIT}
        if set(entries) != offenders:
            raise GateError(
                "bootstrap policy must contain exactly current offenders: "
                f"missing={sorted(offenders-set(entries))}, "
                f"extra={sorted(set(entries)-offenders)}"
            )
        for name, entry in entries.items():
            item = measured[name]
            if entry["manifest"] != item.package.manifest:
                raise GateError(f"bootstrap offender manifest mismatch: {name}")
            if entry["ratchet"] != item.cloc:
                raise GateError(
                    f"bootstrap offender must use exact current CLOC: package={name} count={item.cloc} "
                    f"ratchet={entry['ratchet']}"
                )
        return entries

    previous_entries = parse_policy(previous, "previous accepted")
    if candidate["admission_sha"] != previous["admission_sha"]:
        raise GateError("immutable admission_sha changed from previous accepted policy")
    ownership_transition = is_temporary_ownership_transition(
        entries, previous_entries, measured
    )
    added = set(entries) - set(previous_entries)
    if added:
        raise GateError(
            f"new offender entries are forbidden after bootstrap: added={sorted(added)}"
        )
    removed = set(previous_entries) - set(entries)
    for name in sorted(removed):
        item = measured.get(name)
        if item is not None and item.cloc > HARD_LIMIT:
            raise GateError(
                f"active offender entry removal forbidden: package={name} count={item.cloc} "
                f"limit={HARD_LIMIT} previous_ratchet={previous_entries[name]['ratchet']}"
            )
    for name, entry in entries.items():
        old = previous_entries[name]
        if entry["manifest"] != old["manifest"]:
            raise GateError(f"active offender manifest changed: {name}")
        if entry["ratchet"] > old["ratchet"] and not ownership_transition:
            count = measured[name].cloc if name in measured else 0
            raise GateError(
                f"ratchet raise forbidden: package={name} count={count} limit={HARD_LIMIT} "
                f"ratchet={entry['ratchet']} previous_ratchet={old['ratchet']}"
            )
    return entries


def measurement_failures(
    measurements: list[Measurement], entries: dict[str, dict[str, Any]]
) -> list[str]:
    measured = {item.package.name: item for item in measurements}
    failures: list[str] = []
    for name, item in sorted(measured.items()):
        entry = entries.get(name)
        if entry is None:
            if item.cloc > HARD_LIMIT:
                failures.append(
                    f"package={name} count={item.cloc} limit={HARD_LIMIT} ratchet=none new offender forbidden"
                )
            continue
        if entry["manifest"] != item.package.manifest:
            failures.append(
                f"package={name} count={item.cloc} limit={HARD_LIMIT} "
                f"ratchet={entry['ratchet']} manifest mismatch"
            )
            continue
        if item.cloc <= HARD_LIMIT:
            failures.append(
                f"package={name} count={item.cloc} limit={HARD_LIMIT} ratchet={entry['ratchet']} "
                "retired offender entry must be removed"
            )
            continue
        expected = item.cloc
        if entry["ratchet"] != expected:
            reason = "growth forbidden" if item.cloc > entry["ratchet"] else "stale ratchet after shrink"
            failures.append(
                f"package={name} count={item.cloc} limit={HARD_LIMIT} ratchet={entry['ratchet']} "
                f"expected_ratchet={expected} {reason}"
            )
    for name in sorted(set(entries) - set(measured)):
        entry = entries[name]
        failures.append(
            f"package={name} count=0 limit={HARD_LIMIT} ratchet={entry['ratchet']} "
            "retired offender entry must be removed"
        )
    return failures


def format_failures(failures: list[str]) -> str:
    return (
        "physical Rust crate-size gate failed:\n  "
        + "\n  ".join(failures)
        + "\nAfter legitimate shrink, update the active offender ledger atomically with exactly:\n  "
        + UPDATE_COMMAND
    )


def load_candidate_policy(root: Path) -> dict[str, Any]:
    return read_json(root / POLICY_PATH, "crate-size policy")


def check_checkout(root: Path) -> tuple[list[Measurement], dict[str, Any]]:
    measurements = live_measurements(root)
    candidate = load_candidate_policy(root)
    base, previous = previous_accepted_policy(root)
    entries = validate_policy_transition(candidate, previous, base, measurements)
    failures = measurement_failures(measurements, entries)
    if failures:
        raise GateError(format_failures(failures))
    return measurements, candidate


def updated_policy(
    candidate: dict[str, Any],
    previous: dict[str, Any] | None,
    base_sha: str,
    measurements: list[Measurement],
) -> dict[str, Any]:
    entries = validate_policy_transition(candidate, previous, base_sha, measurements)
    measured = {item.package.name: item for item in measurements}
    updated_entries = []
    for name, entry in sorted(entries.items()):
        item = measured.get(name)
        if item is not None and item.cloc > HARD_LIMIT:
            if item.cloc > entry["ratchet"]:
                raise GateError(
                    f"ratchet raise forbidden: package={name} count={item.cloc} limit={HARD_LIMIT} "
                    f"ratchet={item.cloc} previous_ratchet={entry['ratchet']}"
                )
            updated_entries.append({**entry, "ratchet": item.cloc})
    result = {**candidate, "offenders": updated_entries}
    validate_policy_transition(result, previous, base_sha, measurements)
    failures = measurement_failures(measurements, parse_policy(result, "updated"))
    if failures:
        raise GateError(format_failures(failures))
    return result


def atomic_write_policy(path: Path, value: dict[str, Any]) -> None:
    descriptor, temporary_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    temporary = Path(temporary_name)
    try:
        with os.fdopen(descriptor, "wb") as output:
            output.write(canonical_json(value))
            output.flush()
            os.fsync(output.fileno())
        os.chmod(temporary, path.stat().st_mode)
        os.replace(temporary, path)
    finally:
        if temporary.exists():
            temporary.unlink()


def update_checkout(root: Path) -> None:
    measurements = live_measurements(root)
    candidate = load_candidate_policy(root)
    base, previous = previous_accepted_policy(root)
    replacement = updated_policy(candidate, previous, base, measurements)
    policy_path = root / POLICY_PATH
    if replacement == candidate:
        print("crate-size ratchets already match physical CLOC")
        return
    atomic_write_policy(policy_path, replacement)
    print(f"updated {policy_path.relative_to(root)} atomically")


def resolve_root(value: str) -> Path:
    root = Path(value)
    if not root.is_absolute():
        raise GateError("checkout root must be absolute")
    root = root.resolve()
    if not (root / "Cargo.toml").is_file():
        raise GateError(f"checkout root has no Cargo.toml: {root}")
    return root


def main() -> int:
    if len(sys.argv) != 3 or sys.argv[1] not in {"--preflight", "--update-ratchets"}:
        raise GateError("usage: check-rust-crate-size.py --preflight|--update-ratchets ABSOLUTE_ROOT")
    root = resolve_root(sys.argv[2])
    if sys.argv[1] == "--update-ratchets":
        update_checkout(root)
        return 0
    measurements, candidate = check_checkout(root)
    total_files = sum(item.files for item in measurements)
    total_cloc = sum(item.cloc for item in measurements)
    offenders = parse_policy(candidate, "candidate")
    print(
        f"physical Rust crate-size gate passed: packages={len(measurements)} files={total_files} "
        f"cloc={total_cloc} limit={HARD_LIMIT} offenders={len(offenders)} metric={METRIC}"
    )
    for item in measurements:
        ratchet = offenders.get(item.package.name, {}).get("ratchet", HARD_LIMIT)
        print(f"  {item.package.name}: files={item.files} cloc={item.cloc} ratchet={ratchet}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except GateError as error:
        print(str(error), file=sys.stderr)
        raise SystemExit(1) from None
