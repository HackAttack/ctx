#!/usr/bin/env python3
"""Enforce the permanent 20,000-CLOC production Rust crate limit."""

from __future__ import annotations

from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import sys
from typing import Any, Iterable


POLICY_PATH = "scripts/check-rust-crate-size-policy-v1.json"
BASELINE_SHA256 = "21054056e05623d91157915d9835b4a46ccef41053bea2fa0dfffd79741aa6c0"
SHA256 = re.compile(r"^[0-9a-f]{64}$")
COMMIT = re.compile(r"^[0-9a-f]{40}$")
LABEL = re.compile(r"^//(?:[A-Za-z0-9._+/-]*):[A-Za-z0-9._+/-]+$")
RUST_RULE_KINDS = {"rust_library rule", "rust_binary rule", "rust_proc_macro rule", "rust_test rule"}
CENSUS_CONTROL_PATHS = {
    ".bazelignore",
    ".bazelrc",
    ".bazelversion",
    ".gitignore",
    ".cargo/config",
    ".cargo/config.toml",
    "Cargo.lock",
    "MODULE.bazel",
    "MODULE.bazel.lock",
    "scripts/bazelw",
    "scripts/check.sh",
}


class GateError(RuntimeError):
    pass


@dataclass(frozen=True)
class CargoTarget:
    key: str
    kind: str
    name: str
    root: str


class SourceView:
    def __init__(self, root: Path, paths: set[str], *, allow_symlinks: bool = False):
        self.root = root
        self.paths = paths
        self.allow_symlinks = allow_symlinks

    def exists(self, path: str) -> bool:
        candidate = self.root / path
        return path in self.paths and candidate.is_file() and (self.allow_symlinks or not candidate.is_symlink())

    def read_bytes(self, path: str) -> bytes:
        if not self.exists(path):
            raise GateError(f"declared source is unavailable: {path}")
        try:
            return (self.root / path).read_bytes()
        except OSError as error:
            raise GateError(f"could not read source {path}: {error}") from error

    def read_text(self, path: str, label: str | None = None) -> str:
        try:
            return self.read_bytes(path).decode("utf-8")
        except UnicodeDecodeError as error:
            raise GateError(f"{label or path} is not UTF-8") from error


def canonical_bytes(value: Any) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False) + "\n").encode()


def emit(value: dict[str, Any]) -> None:
    sys.stdout.buffer.write(canonical_bytes(value))


def violation(code: str, detail: str, **fields: Any) -> dict[str, Any]:
    return {"code": code, "detail": detail, **fields}


def normalized_path(value: Any, label: str, *, allow_glob: bool = False) -> str:
    if not isinstance(value, str) or not value:
        raise GateError(f"{label} must be a nonempty path")
    if any(character in value for character in ("\x00", "\t", "\n", "\r", "\\")):
        raise GateError(f"{label} is not normalized: {value!r}")
    if not allow_glob and any(character in value for character in "*?["):
        raise GateError(f"{label} may not contain a glob: {value}")
    path = PurePosixPath(value)
    if path.is_absolute() or value.endswith("/") or any(part in {"", ".", ".."} for part in path.parts):
        raise GateError(f"{label} is not normalized: {value}")
    return value


def git(root: Path, *arguments: str, check: bool = True) -> subprocess.CompletedProcess[bytes]:
    result = subprocess.run(
        ["git", *arguments],
        cwd=root,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if check and result.returncode != 0:
        detail = result.stderr.decode("utf-8", "replace").strip()
        raise GateError(f"git {' '.join(arguments)} failed: {detail}")
    return result


def repo_context() -> tuple[Path, bool]:
    configured = os.environ.get("CTX_CRATE_LOC_ROOT")
    if configured:
        root = Path(configured)
        if not root.is_absolute():
            raise GateError("CTX_CRATE_LOC_ROOT must be absolute")
        root = root.absolute()
        if not (root / "Cargo.toml").is_file():
            raise GateError("crate-size root does not contain Cargo.toml")
        return root, git(root, "rev-parse", "--show-toplevel", check=False).returncode == 0
    result = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode != 0:
        raise GateError("could not locate repository root")
    return Path(result.stdout.decode().strip()).resolve(), True


def repo_file(root: Path, value: Any, label: str) -> Path:
    configured = normalized_path(value, label)
    path = (root / configured).absolute()
    try:
        path.relative_to(root)
    except ValueError as error:
        raise GateError(f"{label} must be inside the repository") from error
    if not path.is_file():
        raise GateError(f"{label} is missing: {configured}")
    return path


def load_json(path: Path, label: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise GateError(f"{label} is not valid UTF-8 JSON: {error}") from error
    if not isinstance(value, dict):
        raise GateError(f"{label} root must be an object")
    return value


def read_policy(root: Path) -> tuple[dict[str, Any], dict[str, Any]]:
    policy = load_json(repo_file(root, POLICY_PATH, "crate-size policy"), "crate-size policy")
    expected = {
        "schema_version",
        "policy",
        "metric_policy",
        "hard_limit",
        "grandfathered_at",
        "previous_accepted_mainline",
        "baseline_inventory",
        "target_inventory",
        "checked_report",
        "exception_ledger",
    }
    if set(policy) != expected or policy.get("schema_version") != 1:
        raise GateError("crate-size policy schema is unsupported")
    if not isinstance(policy.get("policy"), str) or not policy["policy"].strip():
        raise GateError("crate-size policy rationale must be nonempty")
    if policy.get("hard_limit") != 20_000:
        raise GateError("crate-size policy must contain exactly one 20000-CLOC hard limit")
    snapshot = policy.get("grandfathered_at")
    if not isinstance(snapshot, str) or COMMIT.fullmatch(snapshot) is None:
        raise GateError("grandfathered_at must be a full lowercase commit SHA")
    previous = policy.get("previous_accepted_mainline")
    if not isinstance(previous, str) or COMMIT.fullmatch(previous) is None:
        raise GateError("previous_accepted_mainline must be a full lowercase commit SHA")
    metric_policy = load_json(
        repo_file(root, policy.get("metric_policy"), "LOC-v2 metric policy"),
        "LOC-v2 metric policy",
    )
    metric = metric_policy.get("metric")
    if not isinstance(metric, dict):
        raise GateError("LOC-v2 metric policy omits metric")
    return policy, metric


def find_scc(root: Path) -> Path:
    requested = os.environ.get("CTX_CRATE_LOC_SCC") or os.environ.get("CTX_LOC_SCC", "scc")
    candidate = Path(requested)
    if candidate.parent != Path(".") or candidate.is_absolute():
        if not candidate.is_absolute():
            candidate = root / candidate
        resolved = candidate.resolve()
        if not resolved.is_file() or not os.access(resolved, os.X_OK):
            raise GateError(f"pinned scc executable is unavailable: {requested}")
        return resolved
    located = shutil.which(requested)
    if located is None:
        raise GateError("pinned scc executable is unavailable; set CTX_CRATE_LOC_SCC")
    return Path(located).resolve()


def verify_scc(scc: Path, metric: dict[str, Any]) -> dict[str, Any]:
    required = {"tool", "version", "report_field", "archive_sha256", "binary_sha256"}
    if set(metric) != required or metric.get("tool") != "scc" or metric.get("report_field") != "Code":
        raise GateError("LOC-v2 metric configuration is malformed")
    if metric.get("version") != "3.7.0":
        raise GateError("crate-size metric must remain scc 3.7.0")
    for field in ("archive_sha256", "binary_sha256"):
        if not isinstance(metric.get(field), str) or SHA256.fullmatch(metric[field]) is None:
            raise GateError(f"scc {field} pin is malformed")
    actual_hash = hashlib.sha256(scc.read_bytes()).hexdigest()
    if actual_hash != metric["binary_sha256"]:
        raise GateError(f"scc binary hash mismatch: expected {metric['binary_sha256']}, got {actual_hash}")
    result = subprocess.run(
        [str(scc), "--version"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        check=False,
    )
    actual = (result.stdout or result.stderr).strip()
    if result.returncode != 0 or actual != "scc version 3.7.0":
        raise GateError(f"scc version mismatch: expected scc version 3.7.0, got {actual!r}")
    return {"tool": "scc", "version": "3.7.0", "field": "Code"}


def decode_paths(raw: list[bytes], label: str) -> list[str]:
    result: list[str] = []
    for item in raw:
        if not item:
            continue
        try:
            path = item.decode("utf-8")
        except UnicodeDecodeError as error:
            raise GateError(f"{label} paths must be UTF-8") from error
        result.append(normalized_path(path, f"{label} path"))
    return result


def read_paths_manifest(path: Path) -> set[str]:
    if not path.is_absolute() or not path.is_file():
        raise GateError("crate-size source manifest must be an absolute file")
    paths = decode_paths(path.read_bytes().splitlines(), "crate-size source manifest")
    if paths != sorted(paths) or len(paths) != len(set(paths)):
        raise GateError("crate-size source manifest must be sorted and unique")
    return set(paths)


def source_inventory(root: Path, has_git: bool) -> set[str]:
    configured = os.environ.get("CTX_CRATE_LOC_PATHS_MANIFEST")
    if configured:
        result = read_paths_manifest(Path(configured))
    elif has_git:
        result = set(
            decode_paths(
                git(root, "ls-files", "-z", "--cached", "--others", "--exclude-standard").stdout.split(b"\0"),
                "git source inventory",
            )
        )
    else:
        raise GateError("sandboxed crate-size gate requires CTX_CRATE_LOC_PATHS_MANIFEST")
    missing = sorted(path for path in result if not (root / path).is_file())
    if missing:
        raise GateError(f"declared source inventory contains missing files: {missing}")
    return result


def canonicalize_workspace_value(value: Any, root: Path) -> Any:
    marker = "${WORKSPACE}"
    root_text = root.resolve().as_posix()
    if isinstance(value, str):
        return value.replace(root_text, marker)
    if isinstance(value, list):
        return [canonicalize_workspace_value(item, root) for item in value]
    if isinstance(value, dict):
        return {key: canonicalize_workspace_value(item, root) for key, item in value.items()}
    return value


def canonical_cargo_metadata(raw: bytes, root: Path) -> tuple[dict[str, Any], bytes]:
    try:
        value = json.loads(raw)
    except (UnicodeError, json.JSONDecodeError) as error:
        raise GateError(f"cargo metadata returned malformed JSON: {error}") from error
    if not isinstance(value, dict):
        raise GateError("cargo metadata root must be an object")
    normalized = canonicalize_workspace_value(value, root)
    for field in ("target_directory", "build_directory"):
        if field in normalized:
            normalized[field] = "${WORKSPACE}/target"
    return normalized, canonical_bytes(normalized)


def metadata_repo_path(value: Any, label: str) -> str:
    prefix = "${WORKSPACE}/"
    if not isinstance(value, str) or not value.startswith(prefix):
        raise GateError(f"{label} is generated, external, or outside the workspace: {value!r}")
    return normalized_path(value[len(prefix) :], label)


def packages_from_cargo_metadata(metadata: dict[str, Any]) -> list[dict[str, Any]]:
    members = metadata.get("workspace_members")
    records = metadata.get("packages")
    if not isinstance(members, list) or not isinstance(records, list):
        raise GateError("cargo metadata omits workspace packages")
    member_ids = set(members)
    result: list[dict[str, Any]] = []
    for record in records:
        if not isinstance(record, dict) or record.get("id") not in member_ids:
            continue
        name = record.get("name")
        manifest = metadata_repo_path(record.get("manifest_path"), "Cargo package manifest")
        root = PurePosixPath(manifest).parent.as_posix()
        if not isinstance(name, str) or not name or root == ".":
            raise GateError(f"cargo metadata package identity is malformed: {record!r}")
        targets: dict[str, str] = {}
        for target in record.get("targets", []):
            if not isinstance(target, dict):
                raise GateError(f"cargo metadata target is malformed: {name}")
            kinds = target.get("kind")
            target_name = target.get("name")
            if not isinstance(kinds, list) or len(kinds) != 1 or kinds[0] not in {
                "lib", "bin", "test", "example", "bench", "custom-build", "proc-macro"
            }:
                raise GateError(f"unsupported Cargo target kind for {name}: {kinds!r}")
            if not isinstance(target_name, str) or not target_name:
                raise GateError(f"cargo metadata target name is malformed: {name}")
            kind = kinds[0]
            key = f"{kind}:{target_name}"
            path = metadata_repo_path(target.get("src_path"), f"Cargo target source for {name} {key}")
            if key in targets:
                raise GateError(f"duplicate Cargo target identity: {name} {key}")
            targets[key] = path
        result.append({"package": name, "manifest": manifest, "root": root, "cargo_target_roots": dict(sorted(targets.items()))})
    if len(result) != len(member_ids):
        raise GateError("cargo metadata workspace member/package mismatch")
    names = [item["package"] for item in result]
    if len(names) != len(set(names)):
        raise GateError("workspace package names must be unique")
    return sorted(result, key=lambda item: item["package"])


def validate_string_list(value: Any, label: str) -> list[str]:
    if (
        not isinstance(value, list)
        or value != sorted(value)
        or len(value) != len(set(value))
        or any(not isinstance(item, str) or not item for item in value)
    ):
        raise GateError(f"{label} must be a sorted unique string array")
    return value


def validate_label(value: Any, label: str) -> str:
    if not isinstance(value, str) or LABEL.fullmatch(value) is None:
        raise GateError(f"{label} is not an exact main-workspace Bazel label")
    return value


def load_inventory(root: Path, policy: dict[str, Any]) -> dict[str, Any]:
    value = load_json(repo_file(root, policy.get("target_inventory"), "Rust target inventory"), "Rust target inventory")
    if set(value) != {"schema_version", "census", "packages"} or value.get("schema_version") != 4:
        raise GateError("Rust target inventory schema is unsupported")
    if not isinstance(value.get("census"), dict) or not isinstance(value.get("packages"), dict):
        raise GateError("Rust target inventory packages must be an object")
    return value


def natural_package_owner(packages: list[dict[str, Any]], path: str) -> dict[str, Any] | None:
    owners = [package for package in packages if path.startswith(package["root"] + "/")]
    if len(owners) > 1:
        raise GateError(f"overlapping workspace package roots claim {path}: {[item['package'] for item in owners]}")
    return owners[0] if owners else None


def rust_source_owner(packages: list[dict[str, Any]], path: str) -> dict[str, Any]:
    owner = natural_package_owner(packages, path)
    if owner is not None:
        return owner
    embedded = [package for package in packages if f"/{package['root']}/" in f"/{path}"]
    if len(embedded) != 1:
        raise GateError(f"orphan Rust source is not assignable to exactly one workspace package: {path}")
    return embedded[0]


def validate_inventory(
    view: SourceView,
    packages: list[dict[str, Any]],
    inventory: dict[str, Any],
) -> dict[str, dict[str, Any]]:
    declared = inventory["packages"]
    package_names = set(declared)
    result: dict[str, dict[str, Any]] = {}
    expected_keys = {
        "manifest",
        "root",
        "cargo_bazel_targets",
        "cargo_target_roots",
        "bazel_production_targets",
        "native_unit",
        "focused_tests",
    }
    for name in sorted(package_names):
        entry = declared[name]
        if not isinstance(entry, dict) or set(entry) != expected_keys:
            raise GateError(f"Rust target inventory record is malformed: {name}")
        manifest = normalized_path(entry["manifest"], f"manifest for {name}")
        root = normalized_path(entry["root"], f"package root for {name}")
        if manifest != f"{root}/Cargo.toml" or not view.exists(manifest):
            raise GateError(f"Rust target inventory manifest mismatch: {name}")
        build = f"{root}/BUILD.bazel"
        if not view.exists(build):
            raise GateError(f"workspace package has no declared BUILD.bazel: {root}")
        package = {"package": name, "manifest": manifest, "root": root, "build": build}
        roots = entry["cargo_target_roots"]
        targets = entry["cargo_bazel_targets"]
        if not isinstance(targets, dict) or not isinstance(roots, dict):
            raise GateError(f"Rust target inventory target/root mapping is malformed for {name}")
        actual_targets: dict[str, CargoTarget] = {}
        for key, path in roots.items():
            if not isinstance(key, str) or ":" not in key:
                raise GateError(f"Cargo target identity is malformed: {name} {key!r}")
            kind, target_name = key.split(":", 1)
            if kind not in {"lib", "bin", "test", "example", "bench", "custom-build", "proc-macro"} or not target_name:
                raise GateError(f"Cargo target identity is malformed: {name} {key}")
            normalized_path(path, f"Cargo target source for {name} {key}")
            if natural_package_owner([package], path) is None or not view.exists(path):
                raise GateError(f"Cargo target source is missing or outside its package: {name} {key} -> {path}")
            actual_targets[key] = CargoTarget(key, kind, target_name, path)
        expected_mapped = {key for key, target in actual_targets.items() if target.kind != "custom-build"}
        if set(targets) != expected_mapped:
            raise GateError(f"Rust target inventory derived Cargo/Bazel mapping mismatch for {name}")
        labels = [validate_label(value, f"target label for {name} {key}") for key, value in targets.items()]
        if len(labels) != len(set(labels)):
            raise GateError(f"Cargo/Bazel mappings are not one-to-one within {name}")
        production_labels = [
            validate_label(value, f"Bazel production target for {name}")
            for value in validate_string_list(entry["bazel_production_targets"], f"Bazel production targets for {name}")
        ]
        native_unit = entry["native_unit"]
        if native_unit is not None:
            validate_label(native_unit, f"native unit target for {name}")
        for label in validate_string_list(entry["focused_tests"], f"focused tests for {name}"):
            validate_label(label, f"focused test for {name}")
        result[name] = {
            "entry": entry,
            "targets": actual_targets,
            "package": package,
            "bazel_production_targets": set(production_labels),
        }
    packages = [item["package"] for item in result.values()]
    roots = sorted(package["root"] for package in packages)
    for index, root in enumerate(roots):
        if any(other.startswith(root + "/") for other in roots[index + 1 :]):
            raise GateError(f"overlapping workspace package roots are unsupported: {root}")
    manifests = {"Cargo.toml", *(package["manifest"] for package in packages)}
    declared_manifests = {path for path in view.paths if PurePosixPath(path).name == "Cargo.toml"}
    if manifests != declared_manifests:
        raise GateError(
            f"undeclared Cargo manifests: missing={sorted(declared_manifests-manifests)}, stale={sorted(manifests-declared_manifests)}"
        )
    return result


def census_input_paths(paths: set[str]) -> set[str]:
    result: set[str] = set()
    for path in paths:
        name = PurePosixPath(path).name
        if (
            path.endswith(".rs")
            or name in {".gitignore", "Cargo.toml", "BUILD", "BUILD.bazel"}
            or path.endswith(".bzl")
            or path in CENSUS_CONTROL_PATHS
        ):
            result.add(path)
    return result


def git_blob_sha1(content: bytes) -> str:
    header = f"blob {len(content)}\0".encode()
    return hashlib.sha1(header + content).hexdigest()


def file_census_digest(view: SourceView) -> str:
    records = []
    for path in sorted(census_input_paths(view.paths)):
        content = view.read_bytes(path)
        records.append(
            {
                "path": path,
                "git_blob_sha1": git_blob_sha1(content),
                "sha256": hashlib.sha256(content).hexdigest(),
                "size": len(content),
            }
        )
    return hashlib.sha256(canonical_bytes(records)).hexdigest()


def tool_identities(
    root: Path,
    metric_policy: dict[str, Any],
) -> dict[str, str]:
    return {
        "git": "isolated-config ls-files --cached --others --exclude-standard -z; git-blob-sha1",
        "cargo": "cargo-1.97.1 metadata-v1 locked offline no-deps isolated-home-target-config",
        "bazel": repo_file(root, ".bazelversion", "Bazel version pin").read_text(encoding="utf-8").strip(),
        "bazel_query": "local-offline unconfigured XML rust_binary|rust_library|rust_proc_macro|rust_test recursive srcs/filegroups",
        "scc": f"3.7.0:{metric_policy['binary_sha256']}",
    }


def census_components(
    root: Path,
    view: SourceView,
    policy: dict[str, Any],
    metric_policy: dict[str, Any],
    census: dict[str, Any],
    authority_packages: dict[str, Any],
) -> dict[str, Any]:
    cargo_hash = census.get("cargo_metadata_sha256")
    bazel_hash = census.get("bazel_query_sha256")
    accepted_ledger_hash = census.get("accepted_ledger_sha256")
    if not isinstance(cargo_hash, str) or SHA256.fullmatch(cargo_hash) is None:
        raise GateError("Cargo metadata census hash is malformed")
    if not isinstance(bazel_hash, str) or SHA256.fullmatch(bazel_hash) is None:
        raise GateError("Bazel query census hash is malformed")
    if not isinstance(accepted_ledger_hash, str) or SHA256.fullmatch(accepted_ledger_hash) is None:
        raise GateError("accepted mainline ledger census hash is malformed")
    return {
        "schema_version": 1,
        "admission_sha": policy["grandfathered_at"],
        "previous_accepted_mainline": policy["previous_accepted_mainline"],
        "files_sha256": file_census_digest(view),
        "cargo_metadata_sha256": cargo_hash,
        "bazel_query_sha256": bazel_hash,
        "accepted_ledger_sha256": accepted_ledger_hash,
        "authority_packages_sha256": hashlib.sha256(canonical_bytes(authority_packages)).hexdigest(),
        "policy_sha256": hashlib.sha256(repo_file(root, POLICY_PATH, "crate-size policy").read_bytes()).hexdigest(),
        "metric_policy_sha256": hashlib.sha256(
            repo_file(root, policy["metric_policy"], "LOC-v2 metric policy").read_bytes()
        ).hexdigest(),
        "implementation_sha256": hashlib.sha256(
            repo_file(root, "scripts/check-rust-crate-size.py", "crate-size implementation").read_bytes()
        ).hexdigest(),
        "inventory_checker_sha256": hashlib.sha256(
            repo_file(root, "tools/bazel/check_rust_target_inventory.py", "crate-size inventory checker").read_bytes()
        ).hexdigest(),
        "authority_harness_sha256": hashlib.sha256(
            repo_file(root, "scripts/bazel-test.sh", "crate-size authority harness").read_bytes()
        ).hexdigest(),
        "ci_driver_sha256": hashlib.sha256(
            repo_file(root, "scripts/check.sh", "crate-size CI preflight driver").read_bytes()
        ).hexdigest(),
        "tool_identities": tool_identities(root, metric_policy),
    }


def census_full_digest(components: dict[str, Any]) -> str:
    return hashlib.sha256(canonical_bytes(components)).hexdigest()


def validate_census(
    root: Path,
    view: SourceView,
    policy: dict[str, Any],
    metric_policy: dict[str, Any],
    census: dict[str, Any],
    authority_packages: dict[str, Any],
) -> str:
    expected_keys = {
        "schema_version", "admission_sha", "previous_accepted_mainline", "files_sha256",
        "cargo_metadata_sha256", "bazel_query_sha256", "accepted_ledger_sha256", "authority_packages_sha256",
        "policy_sha256", "metric_policy_sha256", "implementation_sha256", "inventory_checker_sha256",
        "authority_harness_sha256", "ci_driver_sha256",
        "tool_identities", "full_sha256",
    }
    if set(census) != expected_keys or census.get("schema_version") != 1:
        raise GateError("crate-size census schema is unsupported")
    components = census_components(root, view, policy, metric_policy, census, authority_packages)
    for key, value in components.items():
        if census.get(key) != value:
            raise GateError(f"crate-size census {key} drift")
    full = census_full_digest(components)
    if census.get("full_sha256") != full:
        raise GateError("crate-size full census digest drift")
    return full


def production_sources(
    view: SourceView,
    packages: list[dict[str, Any]],
    inventory: dict[str, dict[str, Any]],
) -> dict[str, set[str]]:
    result: dict[str, set[str]] = {}
    assigned: dict[str, str] = {}
    for path in sorted(path for path in view.paths if path.endswith(".rs")):
        owner = rust_source_owner(packages, path)
        assigned[path] = owner["package"]
    for package in packages:
        name = package["package"]
        sources = {path for path, owner in assigned.items() if owner == name}
        if not sources:
            raise GateError(f"workspace package has no production Rust source: {name}")
        result[name] = sources
    return result


def run_scc(scc: Path, root: Path, paths: list[str]) -> dict[str, int]:
    if not paths:
        return {}
    result = subprocess.run(
        [
            str(scc),
            "--ci",
            "--by-file",
            "--format",
            "json",
            "--include-symlinks",
            "--no-cocomo",
            "--no-complexity",
            "--no-gitignore",
            "--no-ignore",
            "--no-scc-ignore",
            *paths,
        ],
        cwd=root,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode != 0:
        raise GateError(f"scc failed: {result.stderr.decode('utf-8', 'replace').strip()}")
    try:
        report = json.loads(result.stdout)
    except (UnicodeError, json.JSONDecodeError) as error:
        raise GateError(f"scc returned malformed JSON: {error}") from error
    if not isinstance(report, list):
        raise GateError("scc JSON report root must be an array")
    counts: dict[str, int] = {}
    for language in report:
        files = language.get("Files") if isinstance(language, dict) else None
        if not isinstance(files, list):
            raise GateError("scc JSON report omits by-file data")
        for item in files:
            if not isinstance(item, dict) or not isinstance(item.get("Location"), str):
                raise GateError("scc JSON file record is malformed")
            path = PurePosixPath(item["Location"]).as_posix()
            if path.startswith("./"):
                path = path[2:]
            code = item.get("Code")
            if path in counts or not isinstance(code, int) or isinstance(code, bool) or code < 0:
                raise GateError(f"scc JSON file record is invalid: {path}")
            counts[path] = code
    if set(counts) != set(paths):
        raise GateError(
            f"scc report/source mismatch: missing={sorted(set(paths)-set(counts))}, "
            f"unexpected={sorted(set(counts)-set(paths))}"
        )
    return counts


def inventory_digest(paths: Iterable[str]) -> str:
    return hashlib.sha256(canonical_bytes(sorted(paths))).hexdigest()


def validate_baseline(root: Path, policy: dict[str, Any]) -> dict[str, dict[str, Any]]:
    path = repo_file(root, policy.get("baseline_inventory"), "crate-size baseline inventory")
    actual_hash = hashlib.sha256(path.read_bytes()).hexdigest()
    if actual_hash != BASELINE_SHA256:
        raise GateError(f"immutable crate-size baseline changed: expected {BASELINE_SHA256}, got {actual_hash}")
    value = load_json(path, "crate-size baseline inventory")
    if set(value) != {"schema_version", "snapshot", "hard_limit", "packages"} or value.get("schema_version") != 1:
        raise GateError("crate-size baseline inventory schema is unsupported")
    if value.get("snapshot") != policy["grandfathered_at"] or value.get("hard_limit") != 20_000:
        raise GateError("crate-size baseline identity does not match policy")
    packages = value.get("packages")
    if not isinstance(packages, list):
        raise GateError("crate-size baseline packages must be an array")
    names: list[str] = []
    result: dict[str, dict[str, Any]] = {}
    for item in packages:
        expected = {"package", "manifest", "production_cloc"}
        if not isinstance(item, dict) or set(item) != expected:
            raise GateError("crate-size baseline package record is malformed")
        name = item.get("package")
        manifest = item.get("manifest")
        code = item.get("production_cloc")
        if not isinstance(name, str) or not name or not isinstance(manifest, str):
            raise GateError("crate-size baseline package identity is malformed")
        normalized_path(manifest, f"baseline manifest for {name}")
        if not isinstance(code, int) or isinstance(code, bool) or code < 0:
            raise GateError(f"crate-size baseline CLOC is malformed: {name}")
        names.append(name)
        result[name] = item
    if names != sorted(names) or len(names) != len(set(names)):
        raise GateError("crate-size baseline packages must be sorted and unique")
    return result


def parse_exception_ledger(
    ledger: Any,
    baseline: dict[str, dict[str, Any]],
    label: str,
) -> tuple[dict[str, dict[str, Any]], dict[str, dict[str, Any]]]:
    if not isinstance(ledger, dict) or set(ledger) != {"active", "retired"}:
        raise GateError(f"{label} exception ledger is malformed")
    active_entries = ledger["active"]
    retired_entries = ledger["retired"]
    if not isinstance(active_entries, list) or not isinstance(retired_entries, list):
        raise GateError(f"{label} exception ledger arrays are malformed")
    active: dict[str, dict[str, Any]] = {}
    active_names: list[str] = []
    for entry in active_entries:
        if not isinstance(entry, dict) or set(entry) != {"package", "manifest", "maximum_cloc"}:
            raise GateError(f"{label} active exception is malformed")
        name = entry.get("package")
        ceiling = entry.get("maximum_cloc")
        if not isinstance(name, str) or not name:
            raise GateError(f"{label} active exception package is malformed")
        source = baseline.get(name)
        if source is None or source["production_cloc"] <= 20_000:
            raise GateError(f"new crate-size exceptions are forbidden: {name} was not over limit at admission")
        if entry.get("manifest") != source["manifest"]:
            raise GateError(f"{label} active exception manifest drift: {name}")
        if (
            not isinstance(ceiling, int)
            or isinstance(ceiling, bool)
            or ceiling <= 20_000
            or ceiling > source["production_cloc"]
        ):
            raise GateError(f"{label} active exception ceiling is invalid: {name}")
        active_names.append(name)
        active[name] = entry
    if active_names != sorted(active_names) or len(active_names) != len(set(active_names)):
        raise GateError(f"{label} active exceptions must be sorted and unique")

    retired: dict[str, dict[str, Any]] = {}
    retired_names: list[str] = []
    for entry in retired_entries:
        if not isinstance(entry, dict) or set(entry) != {"package", "manifest", "admission_cloc"}:
            raise GateError(f"{label} retired exception tombstone is malformed")
        name = entry.get("package")
        source = baseline.get(name)
        if not isinstance(name, str) or source is None or source["production_cloc"] <= 20_000:
            raise GateError(f"{label} retired exception was not an admission offender: {name!r}")
        if entry.get("manifest") != source["manifest"] or entry.get("admission_cloc") != source["production_cloc"]:
            raise GateError(f"{label} retired exception tombstone drift: {name}")
        retired_names.append(name)
        retired[name] = entry
    if retired_names != sorted(retired_names) or len(retired_names) != len(set(retired_names)):
        raise GateError(f"{label} retired exception tombstones must be sorted and unique")
    overlap = set(active) & set(retired)
    if overlap:
        raise GateError(f"{label} exception ledger has active/retired overlap: {sorted(overlap)}")
    return active, retired


def validate_ledger(
    policy: dict[str, Any], baseline: dict[str, dict[str, Any]]
) -> tuple[dict[str, dict[str, Any]], dict[str, dict[str, Any]]]:
    return parse_exception_ledger(policy.get("exception_ledger"), baseline, "candidate")


def validate_ledger_transition(
    candidate_policy: dict[str, Any],
    previous_policy: dict[str, Any] | None,
    baseline: dict[str, dict[str, Any]],
) -> str:
    candidate_active, candidate_retired = validate_ledger(candidate_policy, baseline)
    admission_offenders = {
        name for name, record in baseline.items() if record["production_cloc"] > 20_000
    }
    if previous_policy is None:
        if candidate_retired or set(candidate_active) != admission_offenders:
            raise GateError("bootstrap exception ledger must contain exactly the admission offenders and no tombstones")
        for name, entry in candidate_active.items():
            if entry["maximum_cloc"] != baseline[name]["production_cloc"]:
                raise GateError(f"bootstrap exception ceiling must equal immutable admission CLOC: {name}")
        return "bootstrap"

    if previous_policy.get("grandfathered_at") != candidate_policy["grandfathered_at"]:
        raise GateError("immutable exception admission snapshot changed from previous accepted mainline")
    if previous_policy.get("hard_limit") != 20_000:
        raise GateError("previous accepted mainline did not contain the one-tier hard limit")
    previous_active, previous_retired = parse_exception_ledger(
        previous_policy.get("exception_ledger"), baseline, "previous accepted mainline"
    )
    resurrected = set(candidate_active) & set(previous_retired)
    if resurrected:
        raise GateError(f"retired exception cannot be resurrected: {sorted(resurrected)}")
    for name, previous in previous_retired.items():
        if candidate_retired.get(name) != previous:
            raise GateError(f"retired exception tombstone is irreversible: {name}")
    for name, candidate in candidate_active.items():
        previous = previous_active.get(name)
        if previous is None:
            raise GateError(f"new exception is forbidden by previous accepted mainline: {name}")
        if candidate["manifest"] != previous["manifest"]:
            raise GateError(f"exception manifest changed from previous accepted mainline: {name}")
        if candidate["maximum_cloc"] > previous["maximum_cloc"]:
            raise GateError(
                f"exception ceiling increase is forbidden: {name} "
                f"{previous['maximum_cloc']} -> {candidate['maximum_cloc']}"
            )
    removed = set(previous_active) - set(candidate_active)
    expected_retired = set(previous_retired) | removed
    if set(candidate_retired) != expected_retired:
        raise GateError(
            "removed exceptions must become permanent tombstones: "
            f"missing={sorted(expected_retired-set(candidate_retired))}, "
            f"unexpected={sorted(set(candidate_retired)-expected_retired)}"
        )
    for name in removed:
        expected = {
            "package": name,
            "manifest": baseline[name]["manifest"],
            "admission_cloc": baseline[name]["production_cloc"],
        }
        if candidate_retired[name] != expected:
            raise GateError(f"new retired exception tombstone is malformed: {name}")
    return "successor"


def evaluate_limits(
    packages: list[dict[str, Any]],
    exceptions: dict[str, dict[str, Any]],
    hard_limit: int = 20_000,
) -> list[dict[str, Any]]:
    violations: list[dict[str, Any]] = []
    current_names = {item["package"] for item in packages}
    for item in packages:
        name = item["package"]
        code = item["production_cloc"]
        entry = exceptions.get(name)
        if entry is None:
            if code > hard_limit:
                violations.append(
                    violation(
                        "crate_limit",
                        f"{name}: {code} CLOC exceeds the hard production-crate limit {hard_limit}; new exceptions are forbidden",
                        package=name,
                        production_cloc=code,
                        ceiling=hard_limit,
                    )
                )
            continue
        ceiling = entry["maximum_cloc"]
        if item["manifest"] != entry["manifest"]:
            violations.append(violation("exception_manifest_drift", f"{name}: temporary exception manifest changed", package=name))
        elif code <= hard_limit:
            violations.append(
                violation(
                    "stale_exception",
                    f"{name}: {code} CLOC is at or below {hard_limit}; delete the temporary exception",
                    package=name,
                    production_cloc=code,
                    ceiling=ceiling,
                )
            )
        elif code > ceiling:
            violations.append(
                violation(
                    "crate_growth",
                    f"{name}: {code} CLOC exceeds its shrink-only ceiling {ceiling} (+{code-ceiling})",
                    package=name,
                    production_cloc=code,
                    ceiling=ceiling,
                )
            )
        elif code < ceiling:
            violations.append(
                violation(
                    "stale_ceiling",
                    f"{name}: current CLOC is {code}; lower the temporary ceiling from {ceiling} to {code}",
                    package=name,
                    production_cloc=code,
                    ceiling=ceiling,
                )
            )
    for name in sorted(set(exceptions) - current_names):
        violations.append(violation("stale_exception", f"{name}: temporary exception package no longer exists", package=name))
    return violations


def build_package_report(
    packages: list[dict[str, Any]],
    package_sources: dict[str, set[str]],
    counts: dict[str, int],
    exceptions: dict[str, dict[str, Any]],
) -> list[dict[str, Any]]:
    result: list[dict[str, Any]] = []
    for package in packages:
        name = package["package"]
        sources = sorted(package_sources[name])
        code = sum(counts[path] for path in sources)
        entry = exceptions.get(name)
        result.append(
            {
                "package": name,
                "manifest": package["manifest"],
                "production_cloc": code,
                "production_files": len(sources),
                "source_inventory_sha256": inventory_digest(sources),
                "ceiling": entry["maximum_cloc"] if entry else 20_000,
                "policy_status": "temporary-exception" if entry else "over-limit" if code > 20_000 else "within-limit",
            }
        )
    return result


def report_drift(actual: dict[str, Any], expected: dict[str, Any]) -> list[dict[str, Any]]:
    required = {"schema_version", "metric", "hard_limit", "census_sha256", "packages"}
    if set(expected) != required or expected.get("schema_version") != 2:
        raise GateError("checked crate-size report schema is unsupported")
    if actual == expected:
        return []
    violations: list[dict[str, Any]] = []
    expected_packages = {
        item.get("package"): item
        for item in expected.get("packages", [])
        if isinstance(item, dict) and isinstance(item.get("package"), str)
    }
    actual_packages = {item["package"]: item for item in actual["packages"]}
    for name in sorted(set(actual_packages) | set(expected_packages)):
        current = actual_packages.get(name)
        checked = expected_packages.get(name)
        if current != checked:
            violations.append(
                violation(
                    "checked_report_drift",
                    f"{name}: checked per-crate report is stale; record the current deterministic measurement",
                    package=name,
                    checked=checked,
                    actual=current,
                )
            )
    for field in ("metric", "hard_limit", "census_sha256"):
        if actual.get(field) != expected.get(field):
            violations.append(violation("checked_report_drift", f"checked crate-size report {field} is stale"))
    return violations


def main() -> int:
    root, has_git = repo_context()
    policy, metric_policy = read_policy(root)
    scc = find_scc(root)
    metric = verify_scc(scc, metric_policy)
    paths = source_inventory(root, has_git)
    view = SourceView(root, paths, allow_symlinks=os.environ.get("CTX_CRATE_LOC_PATHS_MANIFEST") is not None)
    loaded_inventory = load_inventory(root, policy)
    inventory = validate_inventory(view, [], loaded_inventory)
    packages = [item["package"] for item in inventory.values()]
    census_sha256 = validate_census(
        root, view, policy, metric_policy, loaded_inventory["census"], loaded_inventory["packages"]
    )
    package_sources = production_sources(view, packages, inventory)
    all_sources = sorted(set().union(*package_sources.values()))
    counts = run_scc(scc, root, all_sources)

    baseline = validate_baseline(root, policy)
    exceptions, retired = validate_ledger(policy, baseline)
    packages_report = build_package_report(packages, package_sources, counts, exceptions)
    checked_shape = {
        "schema_version": 2,
        "metric": metric,
        "hard_limit": 20_000,
        "census_sha256": census_sha256,
        "packages": packages_report,
    }
    violations = evaluate_limits(packages_report, exceptions)
    expected = load_json(repo_file(root, policy.get("checked_report"), "checked crate-size report"), "checked crate-size report")
    violations.extend(report_drift(checked_shape, expected))
    violations = sorted(violations, key=canonical_bytes)
    output = {**checked_shape, "status": "fail" if violations else "pass", "violations": violations}
    emit(output)
    if violations:
        print(f"Rust crate-size gate failed with {len(violations)} violation(s):", file=sys.stderr)
        for item in violations:
            print(f"  {item['code']}: {item['detail']}", file=sys.stderr)
        return 1
    print(
        f"Rust crate-size gate passed ({len(packages_report)} production crates; hard limit 20000 CLOC; "
        f"{len(exceptions)} active shrink-only entries; {len(retired)} irreversible tombstones).",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except GateError as error:
        emit(
            {
                "schema_version": 2,
                "status": "error",
                "hard_limit": 20_000,
                "packages": [],
                "violations": [violation("gate_error", str(error))],
            }
        )
        print(f"Rust crate-size gate failed: {error}", file=sys.stderr)
        raise SystemExit(1) from None
