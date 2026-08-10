#!/usr/bin/env python3
"""Generate and verify the Cargo/Bazel authority census for the Rust size gate."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import subprocess
import sys
from typing import Any
import xml.etree.ElementTree as ET


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
        source_labels = labels_from_rule(rule, "srcs") + labels_from_rule(rule, "crate_root")
        expanded: set[str] = set()
        for source in source_labels:
            expanded.update(expand(source))
        sources = sorted(bazel_label_path(gate, source) for source in expanded)
        if not sources:
            raise gate.GateError(f"Bazel Rust target has no resolved checked-in Rust source: {label}")
        records.append({"label": label, "kind": kind, "sources": sources})
    if not records:
        raise gate.GateError("Bazel query found no Rust targets")
    return records, gate.canonical_bytes(records)


def parse_label_kind(gate, raw: bytes) -> dict[str, str]:
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as error:
        raise gate.GateError("Bazel label_kind output is not UTF-8") from error
    result: dict[str, str] = {}
    for line in text.splitlines():
        if not line:
            continue
        try:
            kind, label = line.rsplit(" ", 1)
        except ValueError as error:
            raise gate.GateError(f"malformed Bazel label_kind record: {line!r}") from error
        result[label] = kind
    return result


def expected_rule_kinds(target) -> set[str] | None:
    return {
        "lib": {"rust_library rule", "rust_proc_macro rule"},
        "bin": {"rust_binary rule"},
        "test": {"rust_test rule"},
        "example": {"rust_binary rule", "rust_test rule"},
        "bench": {"rust_test rule"},
    }.get(target.kind)


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
    all_targets: dict[str, str],
    live_paths: set[str],
) -> tuple[dict[str, list[str]], dict[str, set[str]], dict[str, set[str]]]:
    production: dict[str, set[str]] = {package["package"]: set() for package in packages}
    production_sources: dict[str, set[str]] = {package["package"]: set() for package in packages}
    test_sources: dict[str, set[str]] = {package["package"]: set() for package in packages}
    for record in records:
        owner = package_for_bazel_target(gate, packages, record["label"])
        for path in record["sources"]:
            source_owner = gate.natural_package_owner(packages, path)
            if source_owner is None or path not in live_paths:
                raise gate.GateError(f"Bazel Rust source is external, generated, unresolved, or outside a package: {path}")
            destination = test_sources if record["kind"] == "rust_test" else production_sources
            destination[source_owner["package"]].add(path)
        if record["kind"] != "rust_test":
            production[owner["package"]].add(record["label"])
    return (
        {name: sorted(labels) for name, labels in production.items()},
        production_sources,
        test_sources,
    )


def exclusive_test_sources(gate, package, target_records, production_sources, bazel_test_sources) -> list[str]:
    cargo_test_sources = {
        target.root
        for target in target_records.values()
        if gate.standalone_target_root(package, target)
    }
    cargo_production_sources = {
        target.root
        for target in target_records.values()
        if target.kind in {"lib", "bin", "custom-build"}
    }
    return sorted(
        (cargo_test_sources | set(bazel_test_sources))
        - (cargo_production_sources | set(production_sources))
    )


def validate_declared_live_census(gate, declared: set[str], live: set[str]) -> None:
    declared_census = gate.census_input_paths(declared)
    live_census = gate.census_input_paths(live)
    if live_census != declared_census:
        raise gate.GateError(
            "declared/live census drift: "
            f"undeclared={sorted(live_census-declared_census)}, stale={sorted(declared_census-live_census)}"
        )


def inventory_labels(value: dict[str, Any]) -> list[str]:
    labels: set[str] = set()
    for entry in value.get("packages", {}).values():
        if not isinstance(entry, dict):
            continue
        targets = entry.get("targets", {})
        if isinstance(targets, dict):
            labels.update(item for item in targets.values() if isinstance(item, str))
        native = entry.get("native_unit")
        if isinstance(native, str):
            labels.add(native)
        focused = entry.get("focused_tests", [])
        if isinstance(focused, list):
            labels.update(item for item in focused if isinstance(item, str))
    return sorted(labels)


def build_expected_inventory(
    gate,
    root: Path,
    current: dict[str, Any],
    cargo_raw: bytes,
    bazel_raw: bytes,
    label_kind_raw: bytes,
    live_paths: set[str],
    cargo_executable: Path,
) -> dict[str, Any]:
    metadata, metadata_bytes = gate.canonical_cargo_metadata(cargo_raw, root)
    packages = gate.packages_from_cargo_metadata(metadata)
    records, bazel_bytes = parse_bazel_query_xml(gate, bazel_raw)
    all_targets = parse_label_kind(gate, label_kind_raw)
    production, production_sources, bazel_test_sources = validate_graph_and_labels(
        gate, packages, records, all_targets, live_paths
    )
    old_packages = current.get("packages", {})
    if set(old_packages) != {package["package"] for package in packages}:
        raise gate.GateError("Cargo metadata package set differs from the maintained target-label inventory")

    generated_packages: dict[str, Any] = {}
    for package in packages:
        name = package["package"]
        old = old_packages[name]
        targets = old.get("targets")
        if not isinstance(targets, dict) or set(targets) != set(package["cargo_target_roots"]):
            raise gate.GateError(
                f"Cargo metadata target drift for {name}: "
                f"missing={sorted(set(package['cargo_target_roots'])-set(targets or {}))}, "
                f"stale={sorted(set(targets or {})-set(package['cargo_target_roots']))}"
            )
        target_records = {
            key: gate.CargoTarget(key, key.split(":", 1)[0], key.split(":", 1)[1], path)
            for key, path in package["cargo_target_roots"].items()
        }
        for key, label in targets.items():
            kind = all_targets.get(label)
            if kind is None:
                raise gate.GateError(f"Cargo target maps to a missing Bazel target: {name} {key} -> {label}")
            expected = expected_rule_kinds(target_records[key])
            if expected is not None and kind not in expected:
                raise gate.GateError(f"Cargo target maps to the wrong Bazel rule kind: {name} {key} -> {kind}")
        native = old.get("native_unit")
        if native is not None and all_targets.get(native) != "rust_test rule":
            raise gate.GateError(f"{name} native_unit is not an independently proven rust_test: {native}")
        focused = old.get("focused_tests", [])
        if not isinstance(focused, list) or any(all_targets.get(label) != "rust_test rule" for label in focused):
            raise gate.GateError(f"{name} focused_tests contain a non-rust_test target")
        excluded = exclusive_test_sources(
            gate,
            package,
            target_records,
            production_sources[name],
            bazel_test_sources[name],
        )
        generated_packages[name] = {
            "manifest": package["manifest"],
            "root": package["root"],
            "targets": dict(sorted(targets.items())),
            "cargo_target_roots": package["cargo_target_roots"],
            "bazel_production_targets": production[name],
            "excluded_test_sources": excluded,
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
        "tool_identities": {
            "cargo_binary": subprocess.run(
                [str(cargo_executable), "--version"], check=True, text=True, stdout=subprocess.PIPE
            ).stdout.strip(),
            "git_binary": subprocess.run(
                ["git", "--version"], cwd=root, check=True, text=True, stdout=subprocess.PIPE
            ).stdout.strip(),
            "python_binary": f"Python {sys.version.split()[0]}",
        },
    }
    components = gate.census_components(root, view, policy, metric_policy, partial, generated_packages)
    census = {**components, "full_sha256": gate.census_full_digest(components)}
    expected = {"schema_version": 3, "census": census, "packages": generated_packages}
    validated = gate.validate_inventory(view, [], expected)
    gate.production_sources(view, [item["package"] for item in validated.values()], validated)
    return expected


def git_live_paths(gate, root: Path) -> set[str]:
    raw = gate.git(root, "ls-files", "-z", "--cached", "--others", "--exclude-standard").stdout.split(b"\0")
    result = set(gate.decode_paths(raw, "live git census"))
    missing = sorted(path for path in result if not (root / path).is_file())
    if missing:
        raise gate.GateError(f"live git census contains missing files: {missing}")
    return result


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


def live_check(
    root: Path,
    inventory_path: Path,
    paths_manifest: Path,
    cargo_path: Path,
    bazel_path: Path,
    labels_path: Path,
    cargo_executable: Path,
    *,
    render: bool,
) -> None:
    gate = load_gate(root)
    current = read_json(inventory_path)
    live_paths = git_live_paths(gate, root)
    declared = gate.read_paths_manifest(paths_manifest.resolve())
    validate_declared_live_census(gate, declared, live_paths)
    expected = build_expected_inventory(
        gate,
        root,
        current,
        cargo_path.read_bytes(),
        bazel_path.read_bytes(),
        labels_path.read_bytes(),
        live_paths,
        cargo_executable,
    )
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
        f"live Cargo/Bazel census matches {expected['census']['full_sha256']} "
        f"across {len(expected['packages'])} packages"
    )


def main() -> None:
    try:
        if len(sys.argv) == 3 and sys.argv[1] == "--labels":
            print("\n".join(inventory_labels(read_json(Path(sys.argv[2])))))
            return
        if len(sys.argv) == 5 and sys.argv[1] == "--hermetic":
            hermetic_check(Path(sys.argv[2]), Path(sys.argv[3]), Path(sys.argv[4]))
            return
        if len(sys.argv) == 9 and sys.argv[1] in {"--live", "--render"}:
            live_check(
                Path(sys.argv[2]).resolve(),
                Path(sys.argv[3]).resolve(),
                Path(sys.argv[4]).resolve(),
                Path(sys.argv[5]).resolve(),
                Path(sys.argv[6]).resolve(),
                Path(sys.argv[7]).resolve(),
                Path(sys.argv[8]).resolve(),
                render=sys.argv[1] == "--render",
            )
            return
        fail(
            "usage: --labels INVENTORY | --hermetic INVENTORY ROOT_CARGO PATHS_MANIFEST | "
            "--live|--render ROOT INVENTORY PATHS_MANIFEST CARGO_JSON BAZEL_XML LABEL_KIND CARGO"
        )
    except Exception as error:
        if error.__class__.__name__ == "GateError":
            fail(str(error))
        raise


if __name__ == "__main__":
    main()
