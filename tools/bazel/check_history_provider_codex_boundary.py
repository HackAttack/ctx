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


EXPECTED = {
    "base64",
    "chrono",
    "ctx-history-capture-model",
    "ctx-history-capture-runtime",
    "ctx-history-core",
    "ctx-history-provider-runtime",
    "ctx-history-source-io",
    "serde",
    "serde_json",
    "sha2",
    "tempfile",
    "thiserror",
    "uuid",
    "zstd",
}
FORBIDDEN = ("ctx_history_capture::", "ctx_history_index::", "CaptureJsonlRuntime", "IndexBaseEventLookup")


def validate(manifest: Path, build: Path, lib: Path, registration: Path) -> None:
    data = tomllib.loads(manifest.read_text(encoding="utf-8"))
    if data.get("package", {}).get("name") != "ctx-history-provider-codex":
        raise BoundaryError("Codex pack package identity drifted")
    deps = set(data.get("dependencies", {}))
    if deps != EXPECTED:
        raise BoundaryError(f"Codex Cargo dependency inventory drifted: {sorted(deps)}")
    build_text = build.read_text(encoding="utf-8")
    if "//crates/ctx-history-capture:" in build_text or "//crates/ctx-history-index:" in build_text:
        raise BoundaryError("Codex pack gained capture/index Bazel authority")
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
