#!/usr/bin/env python3
"""Enforce the Codex pack's lower-only provider boundary."""

from __future__ import annotations

import argparse
import re
import sys
import tomllib
from pathlib import Path


class BoundaryError(RuntimeError):
    pass


LOWER_INTERNAL_DEPENDENCIES = {
    "ctx-history-capture-model",
    "ctx-history-capture-runtime",
    "ctx-history-core",
    "ctx-history-jsonl",
    "ctx-history-provider-runtime",
}
FORBIDDEN = ("ctx_history_capture::", "ctx_history_index::", "CaptureJsonlRuntime", "IndexBaseEventLookup")


def validate(manifest: Path, build: Path, lib: Path, registration: Path) -> None:
    data = tomllib.loads(manifest.read_text(encoding="utf-8"))
    if data.get("package", {}).get("name") != "ctx-history-provider-codex":
        raise BoundaryError("Codex pack package identity drifted")
    dependencies = data.get("dependencies", {})
    dev_dependencies = data.get("dev-dependencies", {})
    for dependency, contract in dependencies.items():
        if dependency.startswith("ctx-"):
            if dependency not in LOWER_INTERNAL_DEPENDENCIES and not dependency.startswith(
                "ctx-history-source-"
            ):
                raise BoundaryError(f"Codex pack gained an upward internal dependency: {dependency}")
            if contract != {"path": f"../{dependency}"}:
                raise BoundaryError(f"Codex path dependency drifted: {dependency}")
        elif not isinstance(contract, dict) or contract.get("workspace") is not True or "path" in contract:
            raise BoundaryError(f"Codex external dependency must inherit the workspace: {dependency}")
    for dependency, contract in dev_dependencies.items():
        if dependency.startswith("ctx-"):
            if dependency not in dependencies:
                raise BoundaryError(
                    f"Codex dev dependency is not a production-lower dependency: {dependency}"
                )
            if not isinstance(contract, dict) or contract.get("path") != f"../{dependency}":
                raise BoundaryError(f"Codex dev path dependency drifted: {dependency}")
            if set(contract).difference({"path", "features"}):
                raise BoundaryError(f"Codex dev path dependency has unsupported options: {dependency}")
        elif not isinstance(contract, dict) or contract.get("workspace") is not True or "path" in contract:
            raise BoundaryError(
                f"Codex external dev dependency must inherit the workspace: {dependency}"
            )
    build_text = build.read_text(encoding="utf-8")
    if "//crates/ctx-history-capture:" in build_text or "//crates/ctx-history-index:" in build_text:
        raise BoundaryError("Codex pack gained capture/index Bazel authority")
    required_build_labels = {
        f'"//crates/{dependency}:lib"'
        for dependency in dependencies
        if dependency.startswith("ctx-")
    }
    missing_build_labels = sorted(label for label in required_build_labels if label not in build_text)
    if missing_build_labels:
        raise BoundaryError(
            "Codex lower-layer Bazel ownership is incomplete: " + ", ".join(missing_build_labels)
        )
    pack_text = "\n".join(p.read_text(encoding="utf-8") for p in manifest.parent.rglob("*.rs"))
    retained = [fragment for fragment in FORBIDDEN if fragment in pack_text]
    if retained:
        raise BoundaryError("Codex pack retained forbidden seam names: " + ", ".join(retained))
    binding = registration.read_text(encoding="utf-8")
    required = ("CaptureProviderRuntime", "CodexPromptHistoryJsonlFamilyAdapterV0::<CaptureProviderRuntime>")
    missing = [fragment for fragment in required if fragment not in binding]
    if missing:
        raise BoundaryError("capture Codex binding drifted: " + ", ".join(missing))


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("manifest", type=Path)
    parser.add_argument("build", type=Path)
    parser.add_argument("lib", type=Path)
    parser.add_argument("registration", type=Path)
    args = parser.parse_args(argv)
    try:
        validate(args.manifest, args.build, args.lib, args.registration)
    except (BoundaryError, OSError, tomllib.TOMLDecodeError) as error:
        print(error, file=sys.stderr)
        return 1
    print("ctx-history-provider-codex lower-only boundary ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
