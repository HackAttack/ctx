#!/usr/bin/env python3
"""Static ownership and dependency boundary for the SQLite inventory pack."""

from __future__ import annotations

import argparse
from pathlib import Path
import re
import sys
import tomllib


EXPECTED_INTERNAL = {
    "ctx-history-capture-model",
    "ctx-history-capture-runtime",
    "ctx-history-core",
    "ctx-history-provider-runtime",
    "ctx-history-source-discovery",
    "ctx-history-source-io",
    "ctx-history-source-sqlite",
}
FORBIDDEN_PACKAGES = {"ctx-history-capture", "ctx-history-index"}
PROVIDERS = {"astrbot", "crush", "hermes", "lingma", "shelley"}
EXPECTED_REGISTRATIONS = {
    "astrbot_registration",
    "crush_registration",
    "discovered_lingma_registration",
    "hermes_automatic_registration",
    "hermes_explicit_registration",
    "lingma_registration",
    "shelley_registration",
}
EXPECTED_CAPTURE_FACADE_FUNCTIONS = {
    "register_astrbot_source_backed_route",
    "register_crush_source_backed_route",
    "register_hermes_explicit_source_backed_route",
    "register_lingma_source_backed_route",
    "register_shelley_source_backed_route",
}
EXPECTED_CAPTURE_FACADE_REGISTRATIONS = EXPECTED_REGISTRATIONS - {
    "discovered_lingma_registration",
    "hermes_automatic_registration",
}
# Rust identifiers used by this project are ASCII: a letter or underscore,
# followed by letters, digits, or underscores.
RUST_IDENTIFIER = r"[A-Za-z_][A-Za-z0-9_]*"
REGISTRATION_IDENTIFIER = rf"{RUST_IDENTIFIER}_registration"
DEPENDENCY_TABLES = ("dependencies", "dev-dependencies", "build-dependencies")


class BoundaryError(RuntimeError):
    pass


def load_toml(path: Path) -> dict:
    with path.open("rb") as handle:
        return tomllib.load(handle)


def dependency_packages(table: object, label: str) -> set[str]:
    if not isinstance(table, dict):
        raise BoundaryError(f"{label} must be a Cargo dependency table")

    packages = set()
    for dependency, specification in table.items():
        package = dependency
        if isinstance(specification, dict) and "package" in specification:
            package = specification["package"]
            if not isinstance(package, str) or not package:
                raise BoundaryError(
                    f"{label}.{dependency} has an invalid package alias"
                )
        packages.add(package)
    return packages


def dependency_tables(manifest: dict):
    for table_name in DEPENDENCY_TABLES:
        yield table_name, manifest.get(table_name, {})

    targets = manifest.get("target", {})
    if not isinstance(targets, dict):
        raise BoundaryError("target must be a Cargo target table")
    for target, target_config in targets.items():
        if not isinstance(target_config, dict):
            raise BoundaryError(f"target.{target} must be a Cargo target table")
        for table_name in DEPENDENCY_TABLES:
            yield (
                f"target.{target}.{table_name}",
                target_config.get(table_name, {}),
            )


def validate_manifest(path: Path) -> None:
    manifest = load_toml(path)
    normal = manifest.get("dependencies", {})
    normal_packages = dependency_packages(normal, "dependencies")
    internal = {name for name in normal_packages if name.startswith("ctx-")}
    if internal != EXPECTED_INTERNAL:
        raise BoundaryError(
            "SQLite inventory normal dependency inventory drifted: "
            f"expected={sorted(EXPECTED_INTERNAL)} actual={sorted(internal)}"
        )
    all_packages = set()
    for label, table in dependency_tables(manifest):
        all_packages.update(dependency_packages(table, label))
    upward = FORBIDDEN_PACKAGES & all_packages
    if upward:
        raise BoundaryError(
            "SQLite inventory pack gained an upward capture/index dependency: "
            + ", ".join(sorted(upward))
        )
    features = manifest.get("features", {})
    if set(features) != {"test-support"}:
        raise BoundaryError("SQLite inventory feature inventory drifted")


def validate_build(path: Path) -> None:
    text = path.read_text(encoding="utf-8")
    labels = set(re.findall(r'"//crates/(ctx-[^:]+):', text))
    forbidden = FORBIDDEN_PACKAGES & labels
    if forbidden:
        raise BoundaryError(
            "SQLite inventory Bazel target gained an upward edge: "
            + ", ".join(sorted(forbidden))
        )
    if not EXPECTED_INTERNAL <= labels:
        raise BoundaryError(
            "SQLite inventory Bazel lower dependency inventory is incomplete: "
            + ", ".join(sorted(EXPECTED_INTERNAL - labels))
        )


def validate_pack_sources(root: Path) -> None:
    providers = root / "provider/providers/mod.rs"
    declarations = set(
        re.findall(
            rf"pub(?:\(crate\))? mod ({RUST_IDENTIFIER});",
            providers.read_text(encoding="utf-8"),
        )
    )
    if declarations != PROVIDERS:
        raise BoundaryError(
            "SQLite inventory provider ownership drifted: "
            f"expected={sorted(PROVIDERS)} actual={sorted(declarations)}"
        )
    production = []
    for path in root.rglob("*.rs"):
        if path.name == "tests.rs" or path.name.endswith("_tests.rs"):
            continue
        text = path.read_text(encoding="utf-8")
        production.append(strip_test_only_rust_items(text))
    registration_text = strip_rust_non_code("\n".join(production))
    registration_declarations = re.findall(
        r"(?m)^\s*pub(?:\([^)]*\))?\s+"
        rf"(?:async\s+|const\s+|unsafe\s+)*fn\s+({REGISTRATION_IDENTIFIER})\b",
        registration_text,
    )
    actual_registrations = set(registration_declarations)
    duplicate_registrations = {
        registration
        for registration in actual_registrations
        if registration_declarations.count(registration) != 1
    }
    if actual_registrations != EXPECTED_REGISTRATIONS or duplicate_registrations:
        raise BoundaryError(
            "SQLite inventory registration authority drifted: "
            f"missing={sorted(EXPECTED_REGISTRATIONS - actual_registrations)} "
            f"extra={sorted(actual_registrations - EXPECTED_REGISTRATIONS)} "
            f"duplicates={sorted(duplicate_registrations)}"
        )
    joined = "\n".join(production)
    if re.search(r"\bctx_history_(capture|index)\b", joined):
        raise BoundaryError("SQLite inventory production source references capture or index")
    provider_write = re.search(
        r"\b(?:rusqlite::)?Connection::(?:open|open_in_memory)\b"
        r"|\.execute(?:_batch)?\s*\("
        r"|\bpragma_update(?:_and_check)?\s*\("
        r"|\b(?:std::)?fs::write\s*\("
        r"|\bOpenOptions\b",
        joined,
    )
    if provider_write:
        raise BoundaryError(
            "SQLite inventory production source contains a provider write-capable API: "
            + provider_write.group(0)
        )


def strip_test_only_rust_items(text: str) -> str:
    """Remove balanced items gated by Rust's unit-test configuration.

    The boundary is about the shipped library. Rust test modules can be inline
    in otherwise production-named files, so filename filtering alone would
    reject a fixture while missing the same API in a production item.
    """
    marker = re.compile(r"(?m)^\s*#\[cfg\(test\)\]\s*$\n")
    cursor = 0
    production = []
    while match := marker.search(text, cursor):
        production.append(text[cursor : match.start()])
        item_start = match.end()
        brace = next_rust_code_brace(text, item_start)
        if brace < 0:
            # Keep malformed syntax visible to the ordinary source checker.
            production.append(text[item_start:])
            return "".join(production)
        depth = 0
        index = brace
        while index < len(text):
            skipped = skip_rust_non_code(text, index)
            if skipped is not None:
                index = skipped
                continue
            if text[index] == "{":
                depth += 1
            elif text[index] == "}":
                depth -= 1
                if depth == 0:
                    cursor = index + 1
                    break
            index += 1
        else:
            production.append(text[item_start:])
            return "".join(production)
    production.append(text[cursor:])
    return "".join(production)


def next_rust_code_brace(text: str, start: int) -> int:
    index = start
    while index < len(text):
        skipped = skip_rust_non_code(text, index)
        if skipped is not None:
            index = skipped
            continue
        if text[index] == "{":
            return index
        index += 1
    return -1


def skip_rust_non_code(text: str, index: int) -> int | None:
    if text.startswith("//", index):
        newline = text.find("\n", index + 2)
        return len(text) if newline < 0 else newline + 1
    if text.startswith("/*", index):
        depth = 1
        cursor = index + 2
        while cursor < len(text) and depth:
            if text.startswith("/*", cursor):
                depth += 1
                cursor += 2
            elif text.startswith("*/", cursor):
                depth -= 1
                cursor += 2
            else:
                cursor += 1
        return cursor
    raw_string = re.match(r'r(#+)?"', text[index:])
    if raw_string:
        hashes = raw_string.group(1) or ""
        end = text.find('"' + hashes, index + len(raw_string.group(0)))
        return len(text) if end < 0 else end + len(hashes) + 1
    if text[index] == '"':
        cursor = index + 1
        while cursor < len(text):
            if text[cursor] == "\\":
                cursor += 2
            elif text[cursor] == '"':
                return cursor + 1
            else:
                cursor += 1
        return len(text)
    if text[index] == "'":
        cursor = index + 1
        while cursor < len(text):
            if text[cursor] == "\\":
                cursor += 2
            elif text[cursor] == "'":
                return cursor + 1
            elif text[cursor] in "\n\r":
                return None
            else:
                cursor += 1
    return None


def validate_capture_ownership(capture_root: Path) -> None:
    providers = capture_root / "provider/providers/mod.rs"
    declarations = set(
        re.findall(r"pub\(crate\) mod ([a-z_]+);", providers.read_text(encoding="utf-8"))
    )
    retained = PROVIDERS & declarations
    if retained:
        raise BoundaryError(
            "capture retains production ownership of extracted providers: "
            + ", ".join(sorted(retained))
        )
    facade = (
        capture_root
        / "provider/source_backed/registration/families/sqlite_inventory.rs"
    )
    if not facade.is_file():
        raise BoundaryError("capture SQLite inventory façade is missing")
    validate_capture_facade(facade)
    retired = (
        capture_root
        / "provider/source_backed/registration/families/sqlite/inventory.rs"
    )
    if retired.exists():
        raise BoundaryError("capture retains the old SQLite inventory registration owner")


def validate_capture_facade(path: Path) -> None:
    source = strip_rust_non_code(path.read_text(encoding="utf-8"))
    items = set(
        re.findall(
            r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?"
            r"(?:async\s+|const\s+|unsafe\s+)*"
            r"(fn|struct|enum|trait|type|const|static|mod)\s+"
            r"([A-Za-z_][A-Za-z0-9_]*)\b",
            source,
        )
    )
    expected_items = {
        ("fn", function) for function in EXPECTED_CAPTURE_FACADE_FUNCTIONS
    }
    if items != expected_items:
        raise BoundaryError(
            "capture SQLite inventory façade item surface drifted: "
            f"missing={sorted(expected_items - items)} extra={sorted(items - expected_items)}"
        )

    public_items = set(
        re.findall(
            r"(?m)^\s*pub\s+(?:async\s+|const\s+|unsafe\s+)*"
            r"(fn|struct|enum|trait|type|const|static|mod|use)\s+"
            r"([A-Za-z_][A-Za-z0-9_]*)\b",
            source,
        )
    )
    expected_public_items = expected_items
    if public_items != expected_public_items:
        raise BoundaryError(
            "capture SQLite inventory façade public surface drifted: "
            f"expected={sorted(expected_public_items)} actual={sorted(public_items)}"
        )
    restricted_public_items = set(
        re.findall(
            r"(?m)^\s*pub\([^)]*\)\s+(?:async\s+|const\s+|unsafe\s+)*"
            r"(fn|struct|enum|trait|type|const|static|mod|use)\s+"
            r"([A-Za-z_][A-Za-z0-9_]*)\b",
            source,
        )
    )
    if restricted_public_items:
        raise BoundaryError(
            "capture SQLite inventory façade gained a restricted public surface: "
            f"{sorted(restricted_public_items)}"
        )

    registration_calls = re.findall(
        rf"\b({REGISTRATION_IDENTIFIER})\s*(?:::<|\()", source
    )
    registration_calls = [
        registration
        for registration in registration_calls
        if registration != "install_sqlite_inventory_registration"
    ]
    unexpected_calls = set(registration_calls) - EXPECTED_CAPTURE_FACADE_REGISTRATIONS
    duplicate_or_missing_calls = {
        registration: registration_calls.count(registration)
        for registration in EXPECTED_CAPTURE_FACADE_REGISTRATIONS
        if registration_calls.count(registration) != 1
    }
    if unexpected_calls or duplicate_or_missing_calls:
        raise BoundaryError(
            "capture SQLite inventory façade constructor calls drifted: "
            f"unexpected={sorted(unexpected_calls)} "
            f"counts={duplicate_or_missing_calls}"
        )
    registrations = set(
        re.findall(rf"\b({REGISTRATION_IDENTIFIER})\b", source)
    ) - {"install_sqlite_inventory_registration"}
    if registrations != EXPECTED_CAPTURE_FACADE_REGISTRATIONS:
        raise BoundaryError(
            "capture SQLite inventory façade registration bindings drifted: "
            f"missing={sorted(EXPECTED_CAPTURE_FACADE_REGISTRATIONS - registrations)} "
            f"extra={sorted(registrations - EXPECTED_CAPTURE_FACADE_REGISTRATIONS)}"
        )
    if source.count("install_sqlite_inventory_registration(") != len(
        EXPECTED_CAPTURE_FACADE_FUNCTIONS
    ):
        raise BoundaryError(
            "capture SQLite inventory façade must install exactly one pack registration "
            "per compatibility function"
        )


def strip_rust_non_code(text: str) -> str:
    """Blank comments and literals before inspecting Rust declarations and names."""
    code = list(text)
    index = 0
    while index < len(text):
        skipped = skip_rust_non_code(text, index)
        if skipped is None:
            index += 1
            continue
        for cursor in range(index, skipped):
            if code[cursor] not in "\r\n":
                code[cursor] = " "
        index = skipped
    return "".join(code)


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("pack_manifest", type=Path)
    parser.add_argument("pack_build", type=Path)
    parser.add_argument("pack_lib", type=Path)
    parser.add_argument("capture_lib", type=Path)
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    try:
        validate_manifest(args.pack_manifest)
        validate_build(args.pack_build)
        validate_pack_sources(args.pack_lib.parent)
        validate_capture_ownership(args.capture_lib.parent)
    except (BoundaryError, OSError, tomllib.TOMLDecodeError) as error:
        print(error, file=sys.stderr)
        return 1
    print("SQLite inventory provider ownership/dependency boundary ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
