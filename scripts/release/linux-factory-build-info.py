#!/usr/bin/env python3
"""Create deterministic build evidence for the Linux cross-release factory."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess


HEX_40 = re.compile(r"[0-9a-f]{40}")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def target(matrix: Path, platform: str) -> dict[str, object]:
    value = json.loads(matrix.read_text(encoding="utf-8"))
    target_id = "linux-arm64" if platform == "linux-aarch64" else platform
    matches = [item for item in value["targets"] if item["id"] == target_id]
    if len(matches) != 1:
        raise ValueError("release target matrix does not contain the exact platform")
    return matches[0]


def clean_source(repo: Path, commit: str) -> None:
    if HEX_40.fullmatch(commit) is None or commit == "0" * 40:
        raise ValueError("source commit must be nonzero lowercase 40-hex")
    observed = subprocess.check_output(
        ["git", "-C", os.fspath(repo), "rev-parse", "--verify", "HEAD^{commit}"],
        text=True,
    ).strip()
    if observed != commit:
        raise ValueError("source commit does not match the factory checkout")
    status = subprocess.check_output(
        ["git", "-C", os.fspath(repo), "status", "--porcelain=v1"], text=True
    )
    if status:
        raise ValueError("release factory source checkout is dirty")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--artifact", required=True, type=Path)
    parser.add_argument("--cargo-lock", required=True, type=Path)
    parser.add_argument("--matrix", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--platform", required=True)
    parser.add_argument("--recipe", required=True, type=Path)
    parser.add_argument("--rust-version", required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--source-repo", required=True, type=Path)
    parser.add_argument("--static-status", choices=("passed",), required=True)
    parser.add_argument(
        "--local-runtime-status", choices=("passed", "not_run"), required=True
    )
    parser.add_argument(
        "--local-runtime-authority",
        choices=("authoritative", "not_run"),
        required=True,
    )
    parser.add_argument("--zig-version", required=True)
    parser.add_argument("--cargo-zigbuild-version", required=True)
    parser.add_argument("--builder-authority", required=True)
    parser.add_argument("--builder-os", required=True)
    parser.add_argument("--inspector-authority", required=True)
    parser.add_argument("--inspector-tool", required=True)
    parser.add_argument("--macos-sdk-sha256")
    parser.add_argument("--macos-sdk-authority")
    args = parser.parse_args()
    try:
        repo = args.source_repo.resolve(strict=True)
        clean_source(repo, args.source_commit)
        selected = target(args.matrix, args.platform)
        is_macos = selected["os"] == "macos"
        if is_macos != bool(args.macos_sdk_sha256):
            raise ValueError("macOS SDK identity is required exactly for macOS targets")
        if is_macos != bool(args.macos_sdk_authority):
            raise ValueError("macOS SDK authority is required exactly for macOS targets")
        if (args.local_runtime_status == "passed") != (
            args.local_runtime_authority == "authoritative"
        ):
            raise ValueError("runtime status and authority disagree")
        document = {
            "artifact_sha256": sha256(args.artifact),
            "build_system": "cargo-zigbuild",
            "builder": {
                "authority": args.builder_authority,
                "image_id": None,
                "os": args.builder_os,
                "recipe_sha256": sha256(args.recipe),
            },
            "cargo_lock_sha256": sha256(args.cargo_lock),
            "gates": {
                "local_runtime": args.local_runtime_status,
                "local_runtime_authority": args.local_runtime_authority,
                "static": args.static_status,
                "static_abi": args.static_status,
            },
            "inspector": {
                "authority": args.inspector_authority,
                "image_id": None,
                "tool": args.inspector_tool,
            },
            # The shared matrix still carries the legacy Bazel Linux route for
            # old diagnostic consumers. Do not copy its image/sysroot claims
            # into evidence for this Cargo/Zig factory, which did not use them.
            "linux_build": None,
            "platform": args.platform,
            "release_factory": {
                "authority": "linux-cross-cargo-zigbuild-v1",
                "cargo_zigbuild_version": args.cargo_zigbuild_version,
                "glibc_max": selected.get("linux_build", {}).get("glibc_max")
                if isinstance(selected.get("linux_build"), dict)
                else None,
                "macos_sdk_sha256": args.macos_sdk_sha256,
                "macos_sdk_authority": args.macos_sdk_authority,
                "zig_version": args.zig_version,
            },
            "representative_cpu_proof": {"profile": None, "qemu_version": None},
            "runtime": {
                "authority": "native-fanout-deferred-v1",
                "image_id": None,
            },
            "rust_version": args.rust_version,
            "schema_version": 1,
            "source": {"clean": True, "commit": args.source_commit},
            "target": selected["public_rust_target"],
        }
        payload = json.dumps(document, sort_keys=True, separators=(",", ":")) + "\n"
        args.output.parent.mkdir(parents=True, exist_ok=True)
        temporary = args.output.with_name(f".{args.output.name}.tmp.{os.getpid()}")
        temporary.write_text(payload, encoding="utf-8")
        os.replace(temporary, args.output)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        raise SystemExit(f"error: {error}") from error
    print(args.output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
