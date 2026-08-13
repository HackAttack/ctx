#!/usr/bin/env python3
"""Validate an exact release artifact and its matrix-bound build evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import stat
from typing import Any


VERSION = re.compile(
    r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"
)


def regular(path: Path, label: str, maximum: int) -> bytes:
    try:
        metadata = path.lstat()
    except OSError as error:
        raise ValueError(f"{label} is unavailable: {path}") from error
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
        raise ValueError(f"{label} is not a regular file: {path}")
    if metadata.st_size <= 0 or metadata.st_size > maximum:
        raise ValueError(f"{label} has an invalid size: {path}")
    try:
        return path.read_bytes()
    except OSError as error:
        raise ValueError(f"{label} could not be read: {path}") from error


def lower_hex(value: object, length: int) -> bool:
    return isinstance(value, str) and re.fullmatch(
        rf"[0-9a-f]{{{length}}}", value
    ) is not None


def target_by_id(matrix: object, platform: str) -> dict[str, Any]:
    if not isinstance(matrix, dict) or matrix.get("schema_version") != 1:
        raise ValueError("release-target matrix schema is invalid")
    targets = matrix.get("targets")
    if not isinstance(targets, list):
        raise ValueError("release-target matrix targets are invalid")
    target_id = "linux-arm64" if platform == "linux-aarch64" else platform
    matches = [
        target
        for target in targets
        if isinstance(target, dict) and target.get("id") == target_id
    ]
    if len(matches) != 1:
        raise ValueError("release-target matrix does not contain the exact target")
    return matches[0]


def validate(
    artifact: Path,
    build_info_path: Path,
    matrix_path: Path,
    platform: str,
    expected_source_commit: str | None = None,
    cargo_lock_path: Path | None = None,
    factory_inputs_path: Path | None = None,
) -> str:
    release_label = f"{platform} release"
    artifact_bytes = regular(
        artifact, f"{release_label} artifact", 256 * 1024 * 1024
    )
    build_info_bytes = regular(
        build_info_path, f"{release_label} build-info", 64 * 1024
    )
    matrix_bytes = regular(matrix_path, "release-target matrix", 256 * 1024)
    try:
        value = json.loads(build_info_bytes)
        matrix = json.loads(matrix_bytes)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError("release build-info or target matrix is malformed") from error
    target = target_by_id(matrix, platform)
    linux_build = target.get("linux_build")
    if not isinstance(value, dict):
        raise ValueError(f"{release_label} build-info is malformed")

    artifact_sha256 = hashlib.sha256(artifact_bytes).hexdigest()
    cargo_lock_sha256 = (
        hashlib.sha256(
            regular(cargo_lock_path, "Cargo.lock", 32 * 1024 * 1024)
        ).hexdigest()
        if cargo_lock_path is not None
        else None
    )
    source = value.get("source")
    gates = value.get("gates")
    builder = value.get("builder")
    runtime = value.get("runtime")
    inspector = value.get("inspector")
    if (
        value.get("schema_version") != 1
        or value.get("platform") != platform
        or value.get("target") != target.get("public_rust_target")
        or value.get("artifact_sha256") != artifact_sha256
        or not lower_hex(value.get("cargo_lock_sha256"), 64)
        or (
            cargo_lock_sha256 is not None
            and value.get("cargo_lock_sha256") != cargo_lock_sha256
        )
        or not isinstance(source, dict)
        or source.get("clean") is not True
        or not lower_hex(source.get("commit"), 40)
        or source.get("commit") == "0" * 40
        or (
            expected_source_commit is not None
            and source.get("commit") != expected_source_commit
        )
    ):
        raise ValueError(
            f"{release_label} build-info does not bind the clean exact artifact"
        )
    if value.get("linux_build") != linux_build:
        raise ValueError(
            f"{release_label} build-info does not match the matrix build contract"
        )
    if (
        not isinstance(gates, dict)
        or gates.get("static") != "passed"
        or gates.get("static_abi") != "passed"
    ):
        raise ValueError(
            f"{release_label} build-info does not record passed static ABI gates"
        )
    build_info_sha256 = hashlib.sha256(build_info_bytes).hexdigest()
    release_factory = value.get("release_factory")
    if target.get("public_construction_authority") != "linux-cross-cargo-zigbuild-v1":
        raise ValueError(
            f"{release_label} build-info uses a non-factory construction authority"
        )
    if target.get("public_construction_authority") == "linux-cross-cargo-zigbuild-v1":
        expected_sdk_sha256 = None
        try:
            factory_inputs = json.loads(
                (
                    factory_inputs_path
                    or Path("contracts/release-factory-inputs-v1.json")
                ).read_bytes()
            )
            linux_host = factory_inputs["linux_host"]
            if linux_host != {
                "arch": "x86_64",
                "authority": "ctx-release-factory-ubuntu24-x86_64-v1",
                "os_id": "ubuntu",
                "os_version": "24.04",
            }:
                raise ValueError("release factory Linux host contract is invalid")
            expected_builder_authority = linux_host["authority"]
            expected_builder_os = (
                f'{linux_host["os_id"]}-{linux_host["os_version"]}-'
                f'{linux_host["arch"]}'
            )
            if target.get("os") == "macos":
                expected_sdk_sha256 = factory_inputs["macos_sdk"]["archive_sha256"]
                expected_sdk_authority = factory_inputs["macos_sdk"]["authority"]
        except (KeyError, OSError, TypeError, UnicodeDecodeError, json.JSONDecodeError):
            raise ValueError("release factory input contract is unavailable")
        if (
            not isinstance(release_factory, dict)
            or release_factory.get("authority") != "linux-cross-cargo-zigbuild-v1"
            or release_factory.get("zig_version") != "0.15.2"
            or release_factory.get("cargo_zigbuild_version") != "0.23.0"
            or (
                target.get("os") == "macos"
                and release_factory.get("macos_sdk_sha256") != expected_sdk_sha256
            )
            or (
                target.get("os") == "macos"
                and release_factory.get("macos_sdk_authority")
                != expected_sdk_authority
            )
            or (
                target.get("os") != "macos"
                and release_factory.get("macos_sdk_sha256") is not None
            )
        ):
            raise ValueError(
                f"{release_label} build-info does not bind the pinned Linux factory"
            )
        for label, identity in (
            ("builder", builder),
            ("inspector", inspector),
            ("runtime", runtime),
        ):
            if (
                not isinstance(identity, dict)
                or not isinstance(identity.get("authority"), str)
                or not identity["authority"]
            ):
                raise ValueError(f"{release_label} {label} authority is missing")
        if not isinstance(inspector.get("tool"), str) or not inspector["tool"]:
            raise ValueError(f"{release_label} inspector tool identity is missing")
        if (
            builder.get("authority") != expected_builder_authority
            or builder.get("os") != expected_builder_os
        ):
            raise ValueError(
                f"{release_label} builder authority or OS identity is incorrect"
            )
    if target.get("os") != "linux":
        return build_info_sha256

    if (
        gates.get("local_runtime") != "not_run"
        or gates.get("local_runtime_authority") != "not_run"
    ):
        raise ValueError(
            "cross-built Linux build-info must leave native runtime proof to the fan-out"
        )
    return build_info_sha256


def candidate_version(
    artifact: Path,
    build_info_path: Path,
    candidate_manifest_path: Path,
    version_path: Path,
    platform: str,
    build_info_sha256: str,
) -> str:
    artifact_bytes = regular(
        artifact, f"{platform} release artifact", 256 * 1024 * 1024
    )
    build_info_bytes = regular(
        build_info_path, f"{platform} release build-info", 64 * 1024
    )
    candidate_bytes = regular(
        candidate_manifest_path, f"{platform} candidate manifest", 32 * 1024 * 1024
    )
    version_bytes = regular(version_path, f"{platform} construction version", 256)
    try:
        build_info = json.loads(build_info_bytes)
        candidate = json.loads(candidate_bytes)
        version_sidecar = version_bytes.decode("utf-8")
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(
            f"{platform} candidate, build-info, or version sidecar is malformed"
        ) from error

    expected_top = {
        "schema_version",
        "kind",
        "construction",
        "product",
        "version",
        "target",
        "source",
        "artifact",
        "evidence",
        "tantivy",
    }
    expected_target_id = "linux-arm64" if platform == "linux-aarch64" else platform
    version = candidate.get("version") if isinstance(candidate, dict) else None
    target = candidate.get("target") if isinstance(candidate, dict) else None
    artifact_record = candidate.get("artifact") if isinstance(candidate, dict) else None
    evidence = candidate.get("evidence") if isinstance(candidate, dict) else None
    build_info_record = evidence.get("build_info") if isinstance(evidence, dict) else None
    source = build_info.get("source") if isinstance(build_info, dict) else None
    actual_build_info_sha256 = hashlib.sha256(build_info_bytes).hexdigest()
    if (
        not isinstance(candidate, dict)
        or set(candidate) != expected_top
        or candidate.get("schema_version") != 1
        or candidate.get("kind") != "ctx-public-cli-candidate"
        or candidate.get("product") != "core"
        or not isinstance(version, str)
        or VERSION.fullmatch(version) is None
        or not isinstance(target, dict)
        or target.get("id") != expected_target_id
        or target.get("platform") != platform
        or not isinstance(build_info, dict)
        or target.get("rust_triple") != build_info.get("target")
        or candidate.get("source") != source
        or actual_build_info_sha256 != build_info_sha256
        or artifact_record
        != {
            "file": artifact.name,
            "sha256": hashlib.sha256(artifact_bytes).hexdigest(),
            "size_bytes": len(artifact_bytes),
        }
        or build_info_record
        != {"file": build_info_path.name, "sha256": build_info_sha256}
    ):
        raise ValueError(
            f"{platform} candidate manifest does not bind the exact artifact and build-info"
        )

    allowed_sidecars = {
        f"ctx {version}\n",
        f"not run on this host: {platform}\n",
    }
    if version_sidecar not in allowed_sidecars:
        raise ValueError(
            f"{platform} construction version sidecar does not match candidate version {version}"
        )
    return version


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--artifact", type=Path, required=True)
    parser.add_argument("--build-info", type=Path, required=True)
    parser.add_argument("--matrix", type=Path, required=True)
    parser.add_argument("--platform", required=True)
    parser.add_argument("--source-commit")
    parser.add_argument("--cargo-lock", type=Path)
    parser.add_argument("--candidate-manifest", type=Path)
    parser.add_argument("--version-file", type=Path)
    parser.add_argument(
        "--factory-inputs",
        type=Path,
        default=Path("contracts/release-factory-inputs-v1.json"),
    )
    args = parser.parse_args()
    if args.source_commit is not None and (
        not lower_hex(args.source_commit, 40) or args.source_commit == "0" * 40
    ):
        parser.error("--source-commit must be a nonzero lowercase 40-hex commit")
    if (args.candidate_manifest is None) != (args.version_file is None):
        parser.error(
            "--candidate-manifest and --version-file must be supplied together"
        )
    try:
        build_info_sha256 = validate(
            args.artifact,
            args.build_info,
            args.matrix,
            args.platform,
            args.source_commit,
            args.cargo_lock,
            args.factory_inputs,
        )
        if args.candidate_manifest is None:
            print(build_info_sha256)
        else:
            print(
                candidate_version(
                    args.artifact,
                    args.build_info,
                    args.candidate_manifest,
                    args.version_file,
                    args.platform,
                    build_info_sha256,
                )
            )
    except (OSError, ValueError) as error:
        raise SystemExit(f"error: {error}") from error
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
