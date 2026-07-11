#!/usr/bin/env python3
"""Write deterministic, sanitized evidence for one validated release artifact."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
from pathlib import Path


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def optional(value: str) -> str | None:
    return value or None


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--artifact", required=True, type=Path)
    parser.add_argument("--cargo-lock", required=True, type=Path)
    parser.add_argument("--platform", required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--source-clean", required=True, choices=("true", "false"))
    parser.add_argument("--rust-version", required=True)
    parser.add_argument("--expected-builder-base", default="")
    parser.add_argument("--actual-builder-base", default="")
    parser.add_argument("--builder-image-id", default="")
    parser.add_argument("--builder-recipe-sha256", default="")
    parser.add_argument("--runtime-image-id", default="")
    parser.add_argument("--inspector-image-id", default="")
    parser.add_argument("--qemu-version", default="")
    parser.add_argument("--qemu-cpu-profile", default="")
    parser.add_argument("--static-status", required=True, choices=("passed",))
    parser.add_argument("--local-runtime-status", required=True, choices=("passed", "not_run"))
    parser.add_argument(
        "--local-runtime-authority",
        required=True,
        choices=("authoritative", "non_authoritative", "not_run"),
    )
    args = parser.parse_args()

    if bool(args.expected_builder_base) != bool(args.actual_builder_base):
        parser.error("expected and actual builder base identities must be provided together")
    if args.expected_builder_base and args.expected_builder_base != args.actual_builder_base:
        parser.error(
            "resolved builder base identity mismatch: "
            f"expected {args.expected_builder_base}, got {args.actual_builder_base}"
        )
    for label, image_id in (
        ("builder", args.builder_image_id),
        ("runtime", args.runtime_image_id),
        ("inspector", args.inspector_image_id),
    ):
        if image_id and not re.fullmatch(r"sha256:[0-9a-f]{64}", image_id):
            parser.error(f"{label} image identity is not a sha256 digest")
    if (args.local_runtime_status == "not_run") != (
        args.local_runtime_authority == "not_run"
    ):
        parser.error("local runtime authority must be not_run exactly when the gate was not run")

    document = {
        "artifact_sha256": sha256(args.artifact),
        "builder": {
            "base_image": {
                "actual": optional(args.actual_builder_base),
                "expected": optional(args.expected_builder_base),
            },
            "image_id": optional(args.builder_image_id),
            "recipe_sha256": optional(args.builder_recipe_sha256),
        },
        "cargo_lock_sha256": sha256(args.cargo_lock),
        "gates": {
            "local_runtime": args.local_runtime_status,
            "local_runtime_authority": args.local_runtime_authority,
            "static": args.static_status,
        },
        "representative_cpu_proof": {
            "profile": optional(args.qemu_cpu_profile),
            "qemu_version": optional(args.qemu_version),
        },
        "platform": args.platform,
        "rust_version": args.rust_version,
        "schema_version": 1,
        "source": {"clean": args.source_clean == "true", "commit": args.source_commit},
        "target": args.target,
        "runtime": {"image_id": optional(args.runtime_image_id)},
        "inspector": {"image_id": optional(args.inspector_image_id)},
    }
    payload = json.dumps(document, sort_keys=True, separators=(",", ":")) + "\n"
    temporary = args.output.with_name(f".{args.output.name}.tmp.{os.getpid()}")
    try:
        temporary.write_text(payload, encoding="utf-8")
        os.replace(temporary, args.output)
    finally:
        temporary.unlink(missing_ok=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
