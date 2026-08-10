#!/usr/bin/env python3
"""Generate and verify the Cargo/Bazel authority census for the Rust size gate."""

from __future__ import annotations

import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
from typing import Any
import xml.etree.ElementTree as ET


CARGO_VERSION = "1.97.1"
RUST_QUERY = 'kind("rust_(binary|library|proc_macro|test) rule", //...)'
LOCAL_BAZEL_FLAGS = [
    "--repository_disable_download",
    "--remote_executor=",
    "--remote_cache=",
    "--remote_upload_local_results=false",
    "--remote_accept_cached=false",
]


def fail(message: str) -> None:
    raise SystemExit(f"rust target inventory check failed: {message}")


def load_gate(root: Path):
    path = root / "scripts/check-rust-crate-size.py"
    spec = importlib.util.spec_from_file_location("rust_crate_size_authority", path)
    if spec is None or spec.loader is None:
        fail(f"could not load {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def read_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        fail(f"invalid JSON at {path}: {error}")
    if not isinstance(value, dict):
        fail(f"JSON root is not an object: {path}")
    return value


def labels_from_rule(rule: ET.Element, attribute: str) -> list[str]:
    result: list[str] = []
    for child in rule:
        if child.get("name") != attribute:
            continue
        if child.tag == "label" and child.get("value"):
            result.append(child.get("value", ""))
        elif child.tag == "list":
            result.extend(item.get("value", "") for item in child if item.tag == "label")
    return result


def bazel_label_path(gate, label: str) -> str:
    if not label.startswith("//") or ":" not in label:
        raise gate.GateError(f"external or malformed Bazel Rust source: {label}")
    package, target = label[2:].split(":", 1)
    path = f"{package}/{target}" if package else target
    gate.normalized_path(path, "Bazel Rust source")
    if not path.endswith(".rs"):
        raise gate.GateError(f"Bazel Rust source does not resolve to .rs: {label}")
    return path


def parse_bazel_query_xml(gate, raw: bytes) -> tuple[list[dict[str, Any]], bytes]:
    try:
        document = ET.fromstring(raw)
    except ET.ParseError as error:
        raise gate.GateError(f"Bazel query returned malformed XML: {error}") from error
    source_files = {item.get("name") for item in document.findall("source-file") if item.get("name")}
    generated_files = {item.get("name") for item in document.findall("generated-file") if item.get("name")}
    rules = {item.get("name"): item for item in document.findall("rule") if item.get("name")}
    filegroups = {
        label: labels_from_rule(rule, "srcs")
        for label, rule in rules.items()
        if rule.get("class") == "filegroup"
    }

    def expand(label: str, stack: tuple[str, ...] = ()) -> set[str]:
        if label in source_files:
            return {label}
        if label in generated_files:
            raise gate.GateError(f"generated Rust source is forbidden: {label}")
        if label in stack:
            raise gate.GateError(f"Bazel source filegroup cycle: {' -> '.join((*stack, label))}")
        if label in filegroups:
            result: set[str] = set()
            for child in filegroups[label]:
                result.update(expand(child, (*stack, label)))
            return result
        raise gate.GateError(f"unresolved Bazel Rust source/filegroup: {label}")

    records: list[dict[str, Any]] = []
    rust_classes = {"rust_binary", "rust_library", "rust_proc_macro", "rust_test"}
    for label, rule in sorted(rules.items()):
        kind = rule.get("class")
        if kind not in rust_classes:
            continue
        gate.validate_label(label, "Bazel Rust target")
        crate_root_labels = labels_from_rule(rule, "crate_root")
        if len(crate_root_labels) != 1:
            raise gate.GateError(f"Bazel Rust target must declare exactly one crate_root: {label}")
        root_sources = expand(crate_root_labels[0])
        if len(root_sources) != 1:
            raise gate.GateError(f"Bazel Rust crate_root must resolve to exactly one source: {label}")
        crate_root = bazel_label_path(gate, next(iter(root_sources)))
        source_labels = labels_from_rule(rule, "srcs") + crate_root_labels
        expanded: set[str] = set()
        for source in source_labels:
            expanded.update(expand(source))
        sources = sorted(bazel_label_path(gate, source) for source in expanded)
        if not sources:
            raise gate.GateError(f"Bazel Rust target has no resolved checked-in Rust source: {label}")
        records.append({"label": label, "kind": kind, "crate_root": crate_root, "sources": sources})
    if not records:
        raise gate.GateError("Bazel query found no Rust targets")
    return records, gate.canonical_bytes(records)


def expected_rule_kinds(target) -> set[str]:
    return {
        "lib": {"rust_library"},
        "proc-macro": {"rust_proc_macro"},
        "bin": {"rust_binary"},
        "test": {"rust_test"},
        "example": {"rust_binary"},
        "bench": {"rust_test"},
    }[target.kind]


def package_for_bazel_target(gate, packages: list[dict[str, Any]], label: str) -> dict[str, Any]:
    package_path = label[2:].split(":", 1)[0]
    owner = gate.natural_package_owner(packages, package_path + "/__target__")
    if owner is None:
        raise gate.GateError(f"Bazel Rust target is outside every Cargo workspace package: {label}")
    return owner


def validate_graph_and_labels(
    gate,
    packages: list[dict[str, Any]],
    records: list[dict[str, Any]],
    live_paths: set[str],
) -> tuple[dict[str, list[str]], dict[str, dict[str, Any]]]:
    production: dict[str, set[str]] = {package["package"]: set() for package in packages}
    by_label: dict[str, dict[str, Any]] = {}
    for record in records:
        owner = package_for_bazel_target(gate, packages, record["label"])
        for path in record["sources"]:
            source_owner = gate.natural_package_owner(packages, path)
            if source_owner is None or path not in live_paths:
                raise gate.GateError(f"Bazel Rust source is external, generated, unresolved, or outside a package: {path}")
            if source_owner["package"] != owner["package"]:
                raise gate.GateError(
                    f"cross-package Bazel Rust source edge is forbidden: "
                    f"{record['label']} ({owner['package']}) -> {path} ({source_owner['package']})"
                )
        if record["crate_root"] not in record["sources"]:
            raise gate.GateError(f"Bazel crate_root is absent from its declared source closure: {record['label']}")
        if record["kind"] != "rust_test":
            production[owner["package"]].add(record["label"])
        by_label[record["label"]] = {**record, "owner": owner["package"]}
    return {name: sorted(labels) for name, labels in production.items()}, by_label


def derive_cargo_bazel_targets(gate, package, target_records, records_by_label) -> dict[str, str]:
    result: dict[str, str] = {}
    used: set[str] = set()
    for key, target in sorted(target_records.items()):
        if target.kind == "custom-build":
            continue
        matches = [
            record
            for record in records_by_label.values()
            if record["owner"] == package["package"]
            and record["crate_root"] == target.root
            and record["kind"] in expected_rule_kinds(target)
        ]
        if len(matches) != 1:
            raise gate.GateError(
                f"Cargo target must derive exactly one Bazel identity from owner/kind/crate_root: "
                f"{package['package']} {key} -> {[record['label'] for record in matches]}"
            )
        label = matches[0]["label"]
        if label in used:
            raise gate.GateError(f"Cargo/Bazel target identity is not one-to-one: {label}")
        used.add(label)
        result[key] = label
    return result


def validate_cargo_target_ownership(gate, packages, package, target_records) -> None:
    for key, target in target_records.items():
        owner = gate.natural_package_owner(packages, target.root)
        if owner is None or owner["package"] != package["package"]:
            owner_name = None if owner is None else owner["package"]
            raise gate.GateError(
                f"cross-package Cargo Rust source edge is forbidden: "
                f"{package['package']} {key} -> {target.root} ({owner_name})"
            )


def validate_declared_live_census(gate, declared: set[str], live: set[str]) -> None:
    declared_census = gate.census_input_paths(declared)
    live_census = gate.census_input_paths(live)
    if live_census != declared_census:
        raise gate.GateError(
            "declared/live census drift: "
            f"undeclared={sorted(live_census-declared_census)}, stale={sorted(declared_census-live_census)}"
        )


def build_expected_inventory(
    gate,
    root: Path,
    current: dict[str, Any],
    cargo_raw: bytes,
    bazel_raw: bytes,
    live_paths: set[str],
    accepted_ledger_hash: str,
) -> dict[str, Any]:
    metadata, metadata_bytes = gate.canonical_cargo_metadata(cargo_raw, root)
    packages = gate.packages_from_cargo_metadata(metadata)
    records, bazel_bytes = parse_bazel_query_xml(gate, bazel_raw)
    production, records_by_label = validate_graph_and_labels(gate, packages, records, live_paths)
    old_packages = current.get("packages", {})
    if set(old_packages) != {package["package"] for package in packages}:
        raise gate.GateError("Cargo metadata package set differs from the maintained target-label inventory")

    generated_packages: dict[str, Any] = {}
    used_mappings: set[str] = set()
    for package in packages:
        name = package["package"]
        old = old_packages[name]
        if not isinstance(old, dict):
            raise gate.GateError(f"maintained package routing record is malformed: {name}")
        target_records = {
            key: gate.CargoTarget(key, key.split(":", 1)[0], key.split(":", 1)[1], path)
            for key, path in package["cargo_target_roots"].items()
        }
        validate_cargo_target_ownership(gate, packages, package, target_records)
        targets = derive_cargo_bazel_targets(gate, package, target_records, records_by_label)
        duplicate = used_mappings & set(targets.values())
        if duplicate:
            raise gate.GateError(f"Cargo/Bazel target identity is not globally one-to-one: {sorted(duplicate)}")
        used_mappings.update(targets.values())
        native = old.get("native_unit")
        native_record = records_by_label.get(native) if isinstance(native, str) else None
        if native is not None and (
            native_record is None or native_record["kind"] != "rust_test" or native_record["owner"] != name
        ):
            raise gate.GateError(f"{name} native_unit is not an independently proven rust_test: {native}")
        focused = old.get("focused_tests", [])
        if not isinstance(focused, list) or any(
            label not in records_by_label
            or records_by_label[label]["kind"] != "rust_test"
            or records_by_label[label]["owner"] != name
            for label in focused
        ):
            raise gate.GateError(f"{name} focused_tests contain a non-rust_test target")
        generated_packages[name] = {
            "manifest": package["manifest"],
            "root": package["root"],
            "cargo_bazel_targets": dict(sorted(targets.items())),
            "cargo_target_roots": package["cargo_target_roots"],
            "bazel_production_targets": production[name],
            "native_unit": native,
            "focused_tests": sorted(focused),
        }

    view = gate.SourceView(root, live_paths)
    policy, metric_policy = gate.read_policy(root)
    cargo_hash = gate.hashlib.sha256(metadata_bytes).hexdigest()
    bazel_hash = gate.hashlib.sha256(bazel_bytes).hexdigest()
    partial = {
        "cargo_metadata_sha256": cargo_hash,
        "bazel_query_sha256": bazel_hash,
        "accepted_ledger_sha256": accepted_ledger_hash,
    }
    components = gate.census_components(root, view, policy, metric_policy, partial, generated_packages)
    census = {**components, "full_sha256": gate.census_full_digest(components)}
    expected = {"schema_version": 4, "census": census, "packages": generated_packages}
    validated = gate.validate_inventory(view, [], expected)
    gate.production_sources(view, [item["package"] for item in validated.values()], validated)
    return expected


def isolated_git(root: Path, *arguments: str, check: bool = True) -> subprocess.CompletedProcess[bytes]:
    environment = {
        "LC_ALL": "C",
        "PATH": os.environ.get("PATH", os.defpath),
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_OPTIONAL_LOCKS": "0",
    }
    result = subprocess.run(
        [
            "git",
            "-c",
            f"core.excludesFile={os.devnull}",
            "-c",
            "core.ignoreCase=false",
            "-c",
            "core.precomposeUnicode=false",
            *arguments,
        ],
        cwd=root,
        env=environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if check and result.returncode != 0:
        detail = result.stderr.decode("utf-8", "replace").strip()
        raise RuntimeError(f"isolated git {' '.join(arguments)} failed: {detail}")
    return result


def git_live_paths(gate, root: Path) -> set[str]:
    top_level = isolated_git(root, "rev-parse", "--show-toplevel").stdout.decode().strip()
    if Path(top_level).resolve() != root.resolve():
        raise gate.GateError(f"authority preflight requires a local Git checkout root: {root}")
    git_path = isolated_git(root, "rev-parse", "--git-path", "info/exclude").stdout.decode().strip()
    exclude_path = Path(git_path)
    if not exclude_path.is_absolute():
        exclude_path = root / exclude_path
    if exclude_path.is_file():
        live_patterns = [
            line.strip()
            for line in exclude_path.read_text(encoding="utf-8").splitlines()
            if line.strip() and not line.lstrip().startswith("#")
        ]
        if live_patterns:
            raise gate.GateError(f"checkout-local Git info/exclude entries are forbidden: {live_patterns}")
    raw = isolated_git(
        root, "ls-files", "-z", "--cached", "--others", "--exclude-standard"
    ).stdout.split(b"\0")
    result = set(gate.decode_paths(raw, "isolated live git census"))
    missing = sorted(path for path in result if not (root / path).is_file())
    if missing:
        raise gate.GateError(f"live git census contains missing files: {missing}")
    return result


def git_status(root: Path) -> bytes:
    return isolated_git(root, "status", "--porcelain=v1", "-z", "--untracked-files=all").stdout


def accepted_mainline_ledger(gate, root: Path, policy, baseline) -> tuple[str, str]:
    accepted = policy["previous_accepted_mainline"]
    head = isolated_git(root, "rev-parse", "HEAD").stdout.decode().strip()
    origin = isolated_git(root, "rev-parse", "refs/remotes/origin/main").stdout.decode().strip()
    if head == origin:
        expected = isolated_git(root, "rev-parse", "HEAD^1").stdout.decode().strip()
    else:
        expected = isolated_git(root, "merge-base", "HEAD", "refs/remotes/origin/main").stdout.decode().strip()
        if expected != origin:
            raise gate.GateError(
                f"origin/main advanced beyond the candidate merge base: origin={origin}, merge_base={expected}"
            )
    if accepted != expected:
        raise gate.GateError(
            f"previous_accepted_mainline is stale or forged: policy={accepted}, checkout={expected}"
        )
    shown = isolated_git(root, "show", f"{accepted}:{gate.POLICY_PATH}", check=False)
    previous_policy: dict[str, Any] | None
    if shown.returncode == 0:
        try:
            previous_policy = json.loads(shown.stdout)
        except (UnicodeError, json.JSONDecodeError) as error:
            raise gate.GateError(f"previous accepted mainline policy is malformed: {error}") from error
        if not isinstance(previous_policy, dict):
            raise gate.GateError("previous accepted mainline policy root is not an object")
    else:
        previous_policy = None
        if accepted != policy["grandfathered_at"]:
            raise gate.GateError("missing previous mainline ledger is allowed only at immutable admission")
    transition = gate.validate_ledger_transition(policy, previous_policy, baseline)
    accepted_record = {
        "accepted_mainline": accepted,
        "policy_authority": None
        if previous_policy is None
        else {
            "grandfathered_at": previous_policy.get("grandfathered_at"),
            "hard_limit": previous_policy.get("hard_limit"),
            "exception_ledger": previous_policy.get("exception_ledger"),
        },
    }
    digest = gate.hashlib.sha256(gate.canonical_bytes(accepted_record)).hexdigest()
    return transition, digest


def cache_root() -> Path:
    configured = os.environ.get("CTX_BAZEL_CACHE_ROOT")
    if configured:
        return Path(configured).expanduser().resolve()
    xdg = os.environ.get("XDG_CACHE_HOME")
    if xdg:
        return (Path(xdg).expanduser() / "ctx/bazel").resolve()
    home = os.environ.get("HOME")
    if not home:
        raise RuntimeError("HOME, XDG_CACHE_HOME, or CTX_BAZEL_CACHE_ROOT is required for local Bazel discovery")
    return (Path(home).expanduser() / ".cache/ctx/bazel").resolve()


def bazel_command(root: Path, output_root: Path) -> tuple[list[str], dict[str, str], Path]:
    bazelisk = shutil.which("bazelisk") or shutil.which("bazel")
    if bazelisk is None:
        fallback = root / "target/tool-cache/bazelisk/bin/bazelisk"
        if fallback.is_file() and os.access(fallback, os.X_OK):
            bazelisk = str(fallback)
    if bazelisk is None:
        raise RuntimeError("local Bazelisk is required for Rust authority preflight")
    version = (root / ".bazelversion").read_text(encoding="utf-8").strip()
    version_root = cache_root() / f"bazel-{version}"
    repository_cache = Path(
        os.environ.get("CTX_BAZEL_REPOSITORY_CACHE", str(version_root / "repository-cache"))
    ).expanduser().resolve()
    if not repository_cache.is_dir():
        raise RuntimeError(f"offline Bazel repository cache is unavailable: {repository_cache}")
    environment = {
        "LC_ALL": "C",
        "PATH": os.environ.get("PATH", os.defpath),
        "TMPDIR": str(output_root.parent),
        "HOME": str(output_root.parent / "home"),
        "USE_BAZEL_VERSION": version,
        "BAZELISK_HOME": os.environ.get("BAZELISK_HOME", str(cache_root() / "bazelisk")),
    }
    base = [
        bazelisk,
        f"--output_user_root={output_root}",
        "--nosystem_rc",
        "--nohome_rc",
        "--noworkspace_rc",
        f"--bazelrc={root / '.bazelrc'}",
    ]
    return base, environment, repository_cache


def run_bazel(
    root: Path,
    base: list[str],
    environment: dict[str, str],
    repository_cache: Path,
    symlink_prefix: Path,
    command: str,
    *arguments: str,
) -> bytes:
    symlink_flags = [f"--symlink_prefix={symlink_prefix}"] if command in {"build", "cquery"} else []
    result = subprocess.run(
        [
            *base,
            command,
            f"--repository_cache={repository_cache}",
            *symlink_flags,
            *LOCAL_BAZEL_FLAGS,
            *arguments,
        ],
        cwd=root,
        env=environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(
            f"local offline Bazel {command} failed: {result.stderr.decode('utf-8', 'replace').strip()}"
        )
    return result.stdout


def discover_checkout(gate, root: Path, temporary: Path) -> tuple[set[str], bytes, bytes]:
    live_paths = git_live_paths(gate, root)
    output_root = temporary / "bazel-output"
    symlink_prefix = temporary / "bazel-links" / "ctx-"
    symlink_prefix.parent.mkdir()
    (temporary / "home").mkdir()
    base, environment, repository_cache = bazel_command(root, output_root)
    try:
        run_bazel(
            root,
            base,
            environment,
            repository_cache,
            symlink_prefix,
            "build",
            "//tools/bazel:pinned_cargo",
        )
        cargo_output = run_bazel(
            root,
            base,
            environment,
            repository_cache,
            symlink_prefix,
            "cquery",
            "//tools/bazel:pinned_cargo",
            "--output=files",
        ).decode().strip().splitlines()
        if len(cargo_output) != 1:
            raise gate.GateError(f"pinned Cargo cquery returned unexpected files: {cargo_output}")
        execution_root_result = subprocess.run(
            [
                *base,
                "info",
                f"--repository_cache={repository_cache}",
                f"--symlink_prefix={symlink_prefix}",
                *LOCAL_BAZEL_FLAGS,
                "execution_root",
            ],
            cwd=root,
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        if execution_root_result.returncode != 0:
            raise RuntimeError(
                "local Bazel execution_root lookup failed: "
                + execution_root_result.stderr.decode("utf-8", "replace").strip()
            )
        execution_root = Path(execution_root_result.stdout.decode().strip())
        cargo_path = Path(cargo_output[0])
        if not cargo_path.is_absolute():
            cargo_path = execution_root / cargo_path
        cargo_path = cargo_path.resolve(strict=True)
        if not cargo_path.is_file() or not os.access(cargo_path, os.X_OK):
            raise gate.GateError(f"pinned Cargo executable is unavailable: {cargo_path}")

        cargo_environment = {
            "HOME": str(temporary / "cargo-home"),
            "CARGO_HOME": str(temporary / "cargo-home"),
            "CARGO_TARGET_DIR": str(temporary / "cargo-target"),
            "CARGO_NET_OFFLINE": "true",
            "CARGO_TERM_COLOR": "never",
            "CARGO_INCREMENTAL": "0",
            "PATH": os.defpath,
            "LC_ALL": "C",
        }
        (temporary / "cargo-home").mkdir()
        metadata_cwd = temporary / "cargo-metadata-cwd"
        metadata_cwd.mkdir()
        version = subprocess.run(
            [str(cargo_path), "--version"],
            cwd=metadata_cwd,
            env=cargo_environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            check=False,
        )
        match = re.fullmatch(r"cargo ([0-9]+\.[0-9]+\.[0-9]+) \([^\n]+\)\n?", version.stdout)
        if version.returncode != 0 or match is None or match.group(1) != CARGO_VERSION:
            raise gate.GateError(f"pinned Cargo semantic version mismatch: {version.stdout.strip()!r}")
        metadata = subprocess.run(
            [
                str(cargo_path),
                "metadata",
                "--manifest-path",
                str(root / "Cargo.toml"),
                "--locked",
                "--offline",
                "--no-deps",
                "--format-version",
                "1",
            ],
            cwd=metadata_cwd,
            env=cargo_environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        if metadata.returncode != 0:
            raise RuntimeError(
                "isolated Cargo metadata failed: " + metadata.stderr.decode("utf-8", "replace").strip()
            )
        bazel_xml = run_bazel(
            root,
            base,
            environment,
            repository_cache,
            symlink_prefix,
            "query",
            f'{RUST_QUERY} union deps(labels("srcs", {RUST_QUERY})) union labels("crate_root", {RUST_QUERY})',
            "--output=xml",
        )
        return live_paths, metadata.stdout, bazel_xml
    finally:
        subprocess.run(
            [*base, "shutdown"],
            cwd=root,
            env=environment,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        )


def hermetic_check(inventory_path: Path, root_manifest: Path, paths_manifest: Path) -> None:
    root = root_manifest.resolve().parent
    gate = load_gate(root)
    inventory = read_json(inventory_path)
    paths = gate.read_paths_manifest(paths_manifest.resolve())
    view = gate.SourceView(root, paths, allow_symlinks=True)
    policy, metric_policy = gate.read_policy(root)
    validated = gate.validate_inventory(view, [], inventory)
    gate.validate_census(root, view, policy, metric_policy, inventory["census"], inventory["packages"])
    gate.production_sources(view, [item["package"] for item in validated.values()], validated)
    count = sum(len(item["targets"]) for item in validated.values())
    print(f"hermetic Rust authority census owns {count} Cargo targets across {len(validated)} packages")


def checkout_check(root: Path, *, render: bool) -> None:
    gate = load_gate(root)
    inventory_path = root / "tools/bazel/rust-target-inventory.json"
    current = read_json(inventory_path)
    policy, _metric_policy = gate.read_policy(root)
    baseline = gate.validate_baseline(root, policy)
    transition, accepted_ledger_hash = accepted_mainline_ledger(gate, root, policy, baseline)
    before = git_status(root)
    with tempfile.TemporaryDirectory(prefix="ctx-rust-authority-") as temporary_text:
        live_paths, cargo_raw, bazel_raw = discover_checkout(gate, root, Path(temporary_text))
        expected = build_expected_inventory(
            gate,
            root,
            current,
            cargo_raw,
            bazel_raw,
            live_paths,
            accepted_ledger_hash,
        )
    after = git_status(root)
    if after != before:
        raise gate.GateError("checkout authority preflight mutated the live workspace")
    if render:
        print(json.dumps(expected, indent=2, ensure_ascii=False))
        return
    if current != expected:
        current_census = current.get("census", {})
        census_drift = {
            key: {"checked": current_census.get(key), "live": expected["census"].get(key)}
            for key in set(current_census) | set(expected["census"])
            if current_census.get(key) != expected["census"].get(key)
        }
        package_drift = sorted(
            name
            for name in set(current.get("packages", {})) | set(expected["packages"])
            if current.get("packages", {}).get(name) != expected["packages"].get(name)
        )
        raise gate.GateError(
            "checked Rust authority census is stale; regenerate it from the live checkout: "
            f"census={dict(sorted(census_drift.items()))}, packages={package_drift}"
        )
    print(
        f"local checkout Cargo/Bazel census matches {expected['census']['full_sha256']} "
        f"across {len(expected['packages'])} packages; ledger={transition}"
    )


def main() -> None:
    try:
        if len(sys.argv) == 5 and sys.argv[1] == "--hermetic":
            hermetic_check(Path(sys.argv[2]), Path(sys.argv[3]), Path(sys.argv[4]))
            return
        if len(sys.argv) == 3 and sys.argv[1] in {"--preflight", "--render"}:
            checkout_check(Path(sys.argv[2]).resolve(), render=sys.argv[1] == "--render")
            return
        fail(
            "usage: --hermetic INVENTORY ROOT_CARGO PATHS_MANIFEST | --preflight|--render ROOT"
        )
    except Exception as error:
        if error.__class__.__name__ == "GateError":
            fail(str(error))
        if isinstance(error, (OSError, RuntimeError)):
            fail(str(error))
        raise


if __name__ == "__main__":
    main()
