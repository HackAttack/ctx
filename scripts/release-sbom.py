#!/usr/bin/env python3
"""Generate and verify exact-byte public CLI release evidence bundles."""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import re
import stat
import sys
from typing import Any
import zipfile

_SCRIPT_DIRECTORY = os.fspath(Path(__file__).resolve().parent)
sys.path.insert(0, _SCRIPT_DIRECTORY)
try:
    from release_sbom.dependency_materials import (
        BUILD_INFO_CLASSIFICATION,
        HEX_40,
        HEX_64,
        Identity,
        TANTIVY_FEATURES,
        TANTIVY_RESOLVED_FEATURES,
        TANTIVY_VERSION,
        VERSION,
        assert_tantivy_contract,
        canonical,
        cargo_materials,
        material_ref,
        package_identity,
        package_metadata,
        parse_cargo_lock,
        properties,
        regular_bytes,
        selected_adjacency,
        sha256_bytes,
        sha256_file,
        tantivy_closure,
        target_package_identities,
    )
    from release_sbom.generation import build_bundle, load_core_build_info, target_contract
finally:
    del sys.path[0]
del _SCRIPT_DIRECTORY


WINDOWS_TARGET_ID = "windows-x64"
WINDOWS_CONSTRUCTION_ARTIFACT = "ctx.exe"
WINDOWS_RELEASE_ARTIFACT = "ctx-windows-x64.exe"
RELEASE_SUMS = "SHA256SUMS"
WINDOWS_RUNTIME_ARCHIVE = "ctx-onnxruntime-windows-x64.zip"
WINDOWS_RUNTIME_DLL = "lib/onnxruntime.dll"
SUM_LINE = re.compile(r"([0-9a-f]{64})  ([A-Za-z0-9][A-Za-z0-9._-]{0,127})")
MAX_RELEASE_SUMS_BYTES = 64 * 1024
MAX_RUNTIME_ARCHIVE_BYTES = 256 * 1024 * 1024
MAX_RUNTIME_DLL_BYTES = 512 * 1024 * 1024
MAX_RUNTIME_EXPANDED_BYTES = 1024 * 1024 * 1024
LEGACY_RELEASE_ASSETS = (
    "ctx-linux-x64",
    "ctx-linux-x64.cdx.json",
    "ctx-linux-x64.third-party-notices.txt",
    "ctx-linux-aarch64",
    "ctx-linux-aarch64.cdx.json",
    "ctx-linux-aarch64.third-party-notices.txt",
    "ctx-macos-arm64",
    "ctx-macos-arm64.cdx.json",
    "ctx-macos-arm64.third-party-notices.txt",
    "ctx-macos-x64",
    "ctx-macos-x64.cdx.json",
    "ctx-macos-x64.third-party-notices.txt",
    WINDOWS_RELEASE_ARTIFACT,
    f"{WINDOWS_RELEASE_ARTIFACT}.cdx.json",
    f"{WINDOWS_RELEASE_ARTIFACT}.third-party-notices.txt",
    "ctx-onnxruntime-linux-x64.tar.gz",
    "ctx-onnxruntime-linux-aarch64.tar.gz",
    "ctx-onnxruntime-macos-arm64.tar.gz",
    "ctx-onnxruntime-macos-x64.tar.gz",
    WINDOWS_RUNTIME_ARCHIVE,
)
SEMANTIC_RELEASE_ASSETS = (
    "ctx-multilingual-e5-small-onnx-fp32-1.0.0.tar.xz",
    "ctx-multilingual-e5-small-onnx-o4-fp16-1.0.0.tar.xz",
    "ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz",
    "ctx-onnxruntime-linux-x64.tar.zst",
    "ctx-onnxruntime-linux-aarch64.tar.zst",
    "ctx-onnxruntime-macos-arm64.tar.zst",
    "ctx-onnxruntime-macos-x64.tar.zst",
    "ctx-windowsml-windows-x64.zip",
    "ctx-onnxruntime-linux-x64-cuda12.tar.zst",
)
WINDOWS_RUNTIME_FILES = {
    "LICENSE",
    "ThirdPartyNotices.txt",
    "VERSION_NUMBER",
    "GIT_COMMIT_ID",
    "MICROSOFT_VC_RUNTIME_LICENSE.rtf",
    WINDOWS_RUNTIME_DLL,
    "lib/msvcp140.dll",
    "lib/msvcp140_1.dll",
    "lib/vcruntime140.dll",
    "lib/vcruntime140_1.dll",
}
WINDOWS_RUNTIME_ENTRIES = WINDOWS_RUNTIME_FILES | {"lib"}
RELEASE_AUTHORITY_CANDIDATES = (
    "ctx.candidate.json",
    "ctx-linux-aarch64.candidate.json",
    "ctx-macos-arm64.candidate.json",
    "ctx-macos-x64.candidate.json",
    "ctx.exe.candidate.json",
)
WINDOWS_RELEASE_HANDOFF_INPUTS = (
    WINDOWS_CONSTRUCTION_ARTIFACT,
    f"{WINDOWS_CONSTRUCTION_ARTIFACT}.build-info.json",
    f"{WINDOWS_CONSTRUCTION_ARTIFACT}.cdx.json",
    f"{WINDOWS_CONSTRUCTION_ARTIFACT}.size.json",
    f"{WINDOWS_CONSTRUCTION_ARTIFACT}.third-party-notices.txt",
    RELEASE_SUMS,
    WINDOWS_RUNTIME_ARCHIVE,
)
RELEASE_AUTHORITY_HANDOFF_LEAVES = tuple(
    sorted(
        WINDOWS_RELEASE_HANDOFF_INPUTS
        + RELEASE_AUTHORITY_CANDIDATES
        + tuple(f"{name}.sha256" for name in RELEASE_AUTHORITY_CANDIDATES)
    )
)


def release_sums_record(path: Path) -> tuple[dict[str, str], dict[str, object]]:
    if path.name != RELEASE_SUMS:
        raise ValueError(f"release checksum manifest must be named {RELEASE_SUMS}")
    payload = regular_bytes(path, "release SHA256SUMS", MAX_RELEASE_SUMS_BYTES)
    if not payload.endswith(b"\n") or b"\r" in payload or b"\x00" in payload:
        raise ValueError("release SHA256SUMS is not canonical lowercase SHA-256 text")
    try:
        lines = payload.decode("ascii").splitlines()
    except UnicodeDecodeError as error:
        raise ValueError("release SHA256SUMS is not ASCII") from error
    entries: dict[str, str] = {}
    for index, line in enumerate(lines, 1):
        match = SUM_LINE.fullmatch(line)
        if match is None:
            raise ValueError(f"release SHA256SUMS line {index} is malformed")
        digest, name = match.groups()
        if name in entries:
            raise ValueError(f"release SHA256SUMS repeats {name}")
        entries[name] = digest
    names = tuple(entries)
    if names not in (
        LEGACY_RELEASE_ASSETS,
        LEGACY_RELEASE_ASSETS + SEMANTIC_RELEASE_ASSETS,
    ):
        raise ValueError(
            "release SHA256SUMS does not have the exact canonical 20- or "
            "29-entry release inventory and order"
        )
    return entries, {
        "file": RELEASE_SUMS,
        "sha256": sha256_bytes(payload),
        "size_bytes": len(payload),
    }


def safe_zip_name(name: str) -> str:
    parts = name.rstrip("/").split("/")
    if (
        not name
        or "\\" in name
        or name.startswith("/")
        or re.match(r"^[A-Za-z]:", name)
        or any(part in ("", ".", "..") for part in parts)
    ):
        raise ValueError(f"Windows runtime archive has unsafe path {name!r}")
    return "/".join(parts)


def windows_runtime_record(path: Path) -> dict[str, object]:
    if path.name != WINDOWS_RUNTIME_ARCHIVE:
        raise ValueError(
            f"Windows runtime archive must be named {WINDOWS_RUNTIME_ARCHIVE}"
        )
    archive_bytes = regular_bytes(
        path, "Windows runtime archive", MAX_RUNTIME_ARCHIVE_BYTES
    )
    archive_sha256 = sha256_bytes(archive_bytes)
    archive_size = len(archive_bytes)
    try:
        with zipfile.ZipFile(io.BytesIO(archive_bytes)) as archive:
            entries: dict[str, zipfile.ZipInfo] = {}
            expanded_size = 0
            for record in archive.infolist():
                name = safe_zip_name(record.filename)
                if name in entries:
                    raise ValueError(f"Windows runtime archive repeats {name}")
                mode = record.external_attr >> 16
                if record.flag_bits & 1 or stat.S_ISLNK(mode) or mode & 0o7000:
                    raise ValueError(
                        f"Windows runtime archive has unsafe entry {name}"
                    )
                if name not in WINDOWS_RUNTIME_ENTRIES:
                    raise ValueError(
                        f"Windows runtime archive has unexpected entry {name}"
                    )
                if name == "lib":
                    if not record.is_dir():
                        raise ValueError(
                            "Windows runtime archive lib entry is not a directory"
                        )
                elif record.is_dir() or record.file_size <= 0:
                    raise ValueError(
                        f"Windows runtime archive entry is not a non-empty file: {name}"
                    )
                expanded_size += record.file_size
                if expanded_size > MAX_RUNTIME_EXPANDED_BYTES:
                    raise ValueError(
                        "Windows runtime archive exceeds its expanded size limit"
                    )
                entries[name] = record
            if set(entries) != WINDOWS_RUNTIME_ENTRIES:
                missing = sorted(WINDOWS_RUNTIME_ENTRIES - set(entries))
                raise ValueError(
                    "Windows runtime archive entries do not exactly match the "
                    f"legacy sidecar layout; missing: {missing}"
                )
            dll = entries.get(WINDOWS_RUNTIME_DLL)
            if dll is None or dll.is_dir():
                raise ValueError(
                    f"Windows runtime archive does not contain {WINDOWS_RUNTIME_DLL}"
                )
            if dll.file_size <= 0 or dll.file_size > MAX_RUNTIME_DLL_BYTES:
                raise ValueError("Windows runtime DLL has an invalid size")
            digest = hashlib.sha256()
            observed = 0
            with archive.open(dll) as source:
                for chunk in iter(lambda: source.read(1024 * 1024), b""):
                    observed += len(chunk)
                    if observed > MAX_RUNTIME_DLL_BYTES:
                        raise ValueError("Windows runtime DLL exceeds its size limit")
                    digest.update(chunk)
            if observed != dll.file_size:
                raise ValueError("Windows runtime DLL ended before its declared size")
    except (OSError, KeyError, zipfile.BadZipFile) as error:
        raise ValueError("Windows runtime archive is not a valid ZIP") from error
    return {
        "file": WINDOWS_RUNTIME_ARCHIVE,
        "sha256": archive_sha256,
        "size_bytes": archive_size,
        "dll": {
            "file": WINDOWS_RUNTIME_DLL,
            "sha256": digest.hexdigest(),
            "size_bytes": observed,
        },
    }


def atomic_write(path: Path, payload: bytes) -> None:
    temporary = path.with_name(f".{path.name}.tmp.{os.getpid()}")
    try:
        temporary.write_bytes(payload)
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def exclusive_write(path: Path, payload: bytes) -> None:
    descriptor = os.open(
        path,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
        0o600,
    )
    try:
        with os.fdopen(descriptor, "wb", closefd=False) as destination:
            destination.write(payload)
            destination.flush()
            os.fsync(destination.fileno())
    finally:
        os.close(descriptor)


def canonical_path(path: Path) -> Path:
    return Path(os.path.abspath(path)).resolve(strict=False)


def release_handoff_binding(path: Path) -> tuple[int, int, int, int, int, tuple[str, ...]]:
    try:
        metadata = path.lstat()
        names = tuple(sorted(entry.name for entry in path.iterdir()))
    except OSError as error:
        raise ValueError(f"release authority handoff is unavailable: {path}") from error
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
        raise ValueError(f"release authority handoff is not a directory: {path}")
    if names != RELEASE_AUTHORITY_HANDOFF_LEAVES:
        raise ValueError(
            "release authority handoff does not have the exact production inventory"
        )
    return (
        metadata.st_dev,
        metadata.st_ino,
        metadata.st_mode,
        metadata.st_mtime_ns,
        metadata.st_ctime_ns,
        names,
    )


def verify_release_handoff(args: argparse.Namespace) -> str:
    handoff = args.handoff_dir
    before = release_handoff_binding(handoff)
    args.artifact = handoff / WINDOWS_CONSTRUCTION_ARTIFACT
    args.build_info = handoff / f"{WINDOWS_CONSTRUCTION_ARTIFACT}.build-info.json"
    args.sbom = handoff / f"{WINDOWS_CONSTRUCTION_ARTIFACT}.cdx.json"
    args.notices = handoff / f"{WINDOWS_CONSTRUCTION_ARTIFACT}.third-party-notices.txt"
    args.size_report = handoff / f"{WINDOWS_CONSTRUCTION_ARTIFACT}.size.json"
    args.candidate_manifest = handoff / f"{WINDOWS_CONSTRUCTION_ARTIFACT}.candidate.json"
    args.release_sums = handoff / RELEASE_SUMS
    args.runtime_archive = handoff / WINDOWS_RUNTIME_ARCHIVE
    digest = verify_bundle_only(
        args,
        release_bound=True,
        expected_manifest_sha256=args.expected_manifest_sha256,
    )
    if release_handoff_binding(handoff) != before:
        raise ValueError("release authority handoff changed while verified")
    return digest


def read_canonical_json(path: Path, label: str, maximum: int) -> tuple[dict[str, Any], bytes]:
    payload = regular_bytes(path, label, maximum)
    try:
        value = json.loads(payload)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"{label} is malformed") from error
    if not isinstance(value, dict) or canonical(value) != payload:
        raise ValueError(f"{label} is not canonical JSON")
    return value, payload


def verify_bundle_only(
    args: argparse.Namespace,
    *,
    release_bound: bool = False,
    expected_manifest_sha256: str | None = None,
) -> str:
    artifact_sha256 = sha256_file(
        args.artifact, "Core artifact", 256 * 1024 * 1024
    )
    artifact_size = args.artifact.stat().st_size
    candidate, candidate_bytes = read_canonical_json(
        args.candidate_manifest, "candidate manifest", 16 * 1024 * 1024
    )
    candidate_sha256 = sha256_bytes(candidate_bytes)
    if expected_manifest_sha256 is not None:
        if (
            HEX_64.fullmatch(expected_manifest_sha256) is None
            or expected_manifest_sha256 == "0" * 64
        ):
            raise ValueError("expected candidate manifest digest is invalid")
        if candidate_sha256 != expected_manifest_sha256:
            raise ValueError(
                "candidate manifest digest does not match the independently "
                "supplied expected digest"
            )
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
    if release_bound:
        expected_top.update(("release_sums", "runtime"))
    if (
        set(candidate) != expected_top
        or candidate.get("schema_version") != 1
        or candidate.get("kind") != "ctx-public-cli-candidate"
        or candidate.get("product") != "core"
        or candidate.get("artifact")
        != {
            "file": args.artifact.name,
            "sha256": artifact_sha256,
            "size_bytes": artifact_size,
        }
        or candidate.get("construction", {}).get("authority")
        != "linux-cross-cargo-zigbuild-v1"
    ):
        raise ValueError("candidate manifest does not bind the exact construction artifact")
    target = candidate.get("target")
    if (
        not isinstance(target, dict)
        or set(target) != {"id", "platform", "rust_triple"}
        or not all(
            isinstance(target.get(name), str) and target[name]
            for name in ("id", "platform", "rust_triple")
        )
        or candidate.get("version") is None
        or VERSION.fullmatch(str(candidate["version"])) is None
        or target["platform"]
        != ("linux-aarch64" if target["id"] == "linux-arm64" else target["id"])
    ):
        raise ValueError("candidate manifest target is malformed")
    construction = candidate.get("construction")
    authority = construction.get("authority") if isinstance(construction, dict) else None
    label = construction.get("label") if isinstance(construction, dict) else None
    if authority != "linux-cross-cargo-zigbuild-v1" or label != (
        "scripts/release/build-public-candidate-on-linux.sh"
    ):
        raise ValueError("candidate manifest does not bind its target construction route")
    build_info_bytes = regular_bytes(args.build_info, "build-info", 64 * 1024)
    build_info, _ = load_core_build_info(
        build_info_bytes,
        artifact_sha256,
        None,
        str(target["platform"]),
    )
    if (
        candidate.get("source") != build_info["source"]
        or target["rust_triple"] != build_info["target"]
    ):
        raise ValueError("candidate manifest does not bind its exact build-info")
    evidence_paths = {
        "binary_size_report": args.size_report,
        "build_info": args.build_info,
        "cyclonedx_sbom": args.sbom,
        "third_party_notices": args.notices,
    }
    evidence = candidate.get("evidence")
    expected_evidence = {
        "binary_size_report",
        "build_info",
        "candidate_schema",
        "cargo_lock",
        "ctx_history_index_manifest",
        "ctx_history_index_format_manifest",
        "ctx_history_index_query_manifest",
        "cyclonedx_sbom",
        "license_materials_inventory",
        "module_file",
        "module_lock",
        "target_dependency_inventory",
        "target_matrix",
        "third_party_notices",
        "workspace_manifest",
    }
    if not isinstance(evidence, dict) or set(evidence) != expected_evidence:
        raise ValueError("candidate manifest evidence is malformed")
    for name, record in evidence.items():
        if (
            not isinstance(record, dict)
            or set(record) != {"file", "sha256"}
            or not isinstance(record.get("file"), str)
            or not record["file"]
            or HEX_64.fullmatch(str(record.get("sha256"))) is None
        ):
            raise ValueError(f"candidate manifest {name} evidence is malformed")
    for name, path in evidence_paths.items():
        record = evidence.get(name)
        payload = regular_bytes(path, name.replace("_", " "), 32 * 1024 * 1024)
        if record != {"file": path.name, "sha256": sha256_bytes(payload)}:
            raise ValueError(f"candidate manifest does not bind {name}")
    size_report, _ = read_canonical_json(
        args.size_report, "binary size report", 256 * 1024
    )
    if (
        set(size_report)
        != {"artifact", "kind", "product", "schema_version", "target", "version"}
        or size_report.get("schema_version") != 1
        or size_report.get("kind") != "ctx-binary-size-report"
        or size_report.get("product") != candidate["product"]
        or size_report.get("version") != candidate["version"]
        or size_report.get("target") != target
        or size_report.get("artifact") != candidate["artifact"]
    ):
        raise ValueError("binary size report does not bind the exact artifact")
    sbom, _ = read_canonical_json(args.sbom, "CycloneDX SBOM", 16 * 1024 * 1024)
    sbom_root = sbom.get("metadata", {}).get("component", {})
    if (
        sbom.get("bomFormat") != "CycloneDX"
        or sbom_root.get("name") != "ctx"
        or sbom_root.get("version") != candidate["version"]
        or sbom_root.get("hashes")
        != [{"alg": "SHA-256", "content": artifact_sha256}]
    ):
        raise ValueError("CycloneDX SBOM does not bind the exact artifact")
    notices = regular_bytes(
        args.notices, "third-party notices", 32 * 1024 * 1024
    )
    notice_bindings = (
        f"version: {candidate['version']}\n",
        f"target: {target['id']}\n",
        f"platform: {target['platform']}\n",
        f"artifact_sha256: {artifact_sha256}\n",
    )
    if any(binding.encode() not in notices for binding in notice_bindings):
        raise ValueError("third-party notices do not bind the exact artifact")
    tantivy = candidate.get("tantivy")
    closure = tantivy.get("dependency_closure") if isinstance(tantivy, dict) else None
    closure_identities: list[tuple[str, str, str]] = []
    if isinstance(closure, list):
        for package in closure:
            allowed = {"checksum", "license", "name", "source", "version"}
            if (
                not isinstance(package, dict)
                or not {"license", "name", "source", "version"}.issubset(package)
                or not set(package).issubset(allowed)
                or not all(
                    isinstance(package.get(name), str) and package[name]
                    for name in ("license", "name", "source", "version")
                )
                or (
                    "checksum" in package
                    and HEX_64.fullmatch(str(package["checksum"])) is None
                )
            ):
                raise ValueError("candidate manifest Tantivy closure is malformed")
            closure_identities.append(
                (package["name"], package["version"], package["source"])
            )
    if (
        not isinstance(tantivy, dict)
        or tantivy.get("version") != TANTIVY_VERSION
        or tantivy.get("default_features") is not False
        or tantivy.get("features") != TANTIVY_FEATURES
        or tantivy.get("resolved_crate_features") != TANTIVY_RESOLVED_FEATURES
        or HEX_64.fullmatch(str(tantivy.get("dependency_closure_sha256"))) is None
        or not closure_identities
        or closure_identities != sorted(set(closure_identities))
        or sha256_bytes(canonical(closure))
        != tantivy.get("dependency_closure_sha256")
        or ("tantivy", TANTIVY_VERSION)
        not in {(name, version) for name, version, _ in closure_identities}
        or {"fs4", "lz4_flex", "memmap2", "tempfile", "zstd"}
        - {name for name, _, _ in closure_identities}
        or any(name == "rust-stemmers" for name, _, _ in closure_identities)
    ):
        raise ValueError("candidate manifest Tantivy contract is malformed")
    if release_bound:
        if (
            target["id"] != WINDOWS_TARGET_ID
            or candidate["artifact"]["file"] != WINDOWS_CONSTRUCTION_ARTIFACT
        ):
            raise ValueError(
                "release-bound candidate manifest is not the Windows factory candidate"
            )
        sums, sums_record = release_sums_record(args.release_sums)
        runtime_record = windows_runtime_record(args.runtime_archive)
        if candidate.get("release_sums") != sums_record:
            raise ValueError("candidate manifest does not bind exact release SHA256SUMS")
        if candidate.get("runtime") != runtime_record:
            raise ValueError("candidate manifest does not bind exact Windows runtime and DLL")
        if sums[WINDOWS_RELEASE_ARTIFACT] != artifact_sha256:
            raise ValueError(
                f"release SHA256SUMS does not bind {WINDOWS_RELEASE_ARTIFACT} "
                "to the candidate artifact"
            )
        if sums[WINDOWS_RUNTIME_ARCHIVE] != runtime_record["sha256"]:
            raise ValueError(
                f"release SHA256SUMS does not bind {WINDOWS_RUNTIME_ARCHIVE} "
                "to the candidate runtime"
            )
    return candidate_sha256


def bind_release_candidate(args: argparse.Namespace) -> tuple[bytes, bytes]:
    verified_candidate_sha256 = verify_bundle_only(args)
    candidate, candidate_bytes = read_canonical_json(
        args.candidate_manifest, "candidate manifest", 16 * 1024 * 1024
    )
    if sha256_bytes(candidate_bytes) != verified_candidate_sha256:
        raise ValueError("candidate manifest changed while release binding was verified")
    target = candidate.get("target")
    artifact = candidate.get("artifact")
    if (
        not isinstance(target, dict)
        or target.get("id") != WINDOWS_TARGET_ID
        or target.get("platform") != WINDOWS_TARGET_ID
        or not isinstance(artifact, dict)
        or artifact.get("file") != WINDOWS_CONSTRUCTION_ARTIFACT
    ):
        raise ValueError(
            "release binding requires the exact Windows factory construction candidate"
        )
    sums, sums_record = release_sums_record(args.release_sums)
    runtime_record = windows_runtime_record(args.runtime_archive)
    if sums[WINDOWS_RELEASE_ARTIFACT] != artifact["sha256"]:
        raise ValueError(
            f"release SHA256SUMS does not bind {WINDOWS_RELEASE_ARTIFACT} "
            "to the candidate artifact"
        )
    if sums[WINDOWS_RUNTIME_ARCHIVE] != runtime_record["sha256"]:
        raise ValueError(
            f"release SHA256SUMS does not bind {WINDOWS_RUNTIME_ARCHIVE} "
            "to the candidate runtime"
        )
    candidate["release_sums"] = sums_record
    candidate["runtime"] = runtime_record
    payload = canonical(candidate)
    digest = sha256_bytes(payload)
    return payload, f"{digest}\n".encode("ascii")


def require_full_arguments(parser: argparse.ArgumentParser, args: argparse.Namespace) -> None:
    names = (
        "artifact",
        "build_info",
        "candidate_manifest",
        "candidate_schema",
        "cargo_lock",
        "index_manifest",
        "index_format_manifest",
        "index_query_manifest",
        "license_materials",
        "module_file",
        "module_lock",
        "notices",
        "notices_output",
        "output",
        "platform",
        "product",
        "sbom",
        "size_report",
        "size_report_output",
        "target_id",
        "target_inventory",
        "target_matrix",
        "version",
        "workspace_manifest",
    )
    generate_only = {"notices_output", "output", "size_report_output"}
    verify_only = {"notices", "sbom", "size_report"}
    missing = []
    for name in names:
        if args.mode == "generate" and name in verify_only:
            continue
        if args.mode == "verify" and name in generate_only:
            continue
        if getattr(args, name) is None:
            missing.append("--" + name.replace("_", "-"))
    if missing:
        parser.error(f"{args.mode} requires " + ", ".join(missing))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "mode",
        choices=(
            "generate",
            "verify",
            "verify-bundle",
            "bind-release",
            "verify-release",
        ),
    )
    parser.add_argument("--product", choices=("core",))
    parser.add_argument("--version")
    parser.add_argument("--target-id")
    parser.add_argument("--platform")
    parser.add_argument("--artifact", type=Path)
    parser.add_argument("--build-info", type=Path)
    parser.add_argument("--cargo-lock", type=Path)
    parser.add_argument("--module-lock", type=Path)
    parser.add_argument("--module-file", type=Path)
    parser.add_argument("--target-inventory", type=Path)
    parser.add_argument("--license-materials", type=Path)
    parser.add_argument("--target-matrix", type=Path)
    parser.add_argument("--candidate-schema", type=Path)
    parser.add_argument("--workspace-manifest", type=Path)
    parser.add_argument("--index-manifest", type=Path)
    parser.add_argument("--index-format-manifest", type=Path)
    parser.add_argument("--index-query-manifest", type=Path)
    parser.add_argument("--runfiles-root", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--notices-output", type=Path)
    parser.add_argument("--size-report-output", type=Path)
    parser.add_argument("--candidate-manifest", type=Path)
    parser.add_argument("--sbom", type=Path)
    parser.add_argument("--notices", type=Path)
    parser.add_argument("--size-report", type=Path)
    parser.add_argument("--release-sums", type=Path)
    parser.add_argument("--runtime-archive", type=Path)
    parser.add_argument("--output-manifest", type=Path)
    parser.add_argument("--manifest-sha256-output", type=Path)
    parser.add_argument("--expected-manifest-sha256")
    parser.add_argument("--handoff-dir", type=Path)
    args = parser.parse_args()
    try:
        if args.mode == "verify-release":
            required = ("handoff_dir", "expected_manifest_sha256")
            missing = [
                "--" + name.replace("_", "-")
                for name in required
                if getattr(args, name) is None
            ]
            if missing:
                parser.error(f"verify-release requires " + ", ".join(missing))
            explicit_inputs = (
                "artifact",
                "build_info",
                "candidate_manifest",
                "notices",
                "release_sums",
                "runtime_archive",
                "sbom",
                "size_report",
            )
            if any(getattr(args, name) is not None for name in explicit_inputs):
                parser.error(
                    "verify-release accepts release inputs only through --handoff-dir"
                )
            print(verify_release_handoff(args))
            return 0

        if args.mode in ("verify-bundle", "bind-release"):
            required = (
                "artifact",
                "build_info",
                "candidate_manifest",
                "notices",
                "sbom",
                "size_report",
            )
            if args.mode == "bind-release":
                required += ("release_sums", "runtime_archive")
                required += ("output_manifest", "manifest_sha256_output")
            missing = [
                "--" + name.replace("_", "-")
                for name in required
                if getattr(args, name) is None
            ]
            if missing:
                parser.error(f"{args.mode} requires " + ", ".join(missing))
            if args.mode == "verify-bundle":
                print(verify_bundle_only(args))
            elif args.mode == "bind-release":
                outputs = (args.output_manifest, args.manifest_sha256_output)
                inputs = (
                    args.artifact,
                    args.build_info,
                    args.candidate_manifest,
                    args.notices,
                    args.release_sums,
                    args.runtime_archive,
                    args.sbom,
                    args.size_report,
                )
                canonical_outputs = tuple(canonical_path(path) for path in outputs)
                canonical_inputs = {canonical_path(path) for path in inputs}
                if (
                    len(set(canonical_outputs)) != len(canonical_outputs)
                    or canonical_inputs.intersection(canonical_outputs)
                ):
                    parser.error("bind-release inputs and outputs must be distinct")
                manifest, digest = bind_release_candidate(args)
                exclusive_write(args.output_manifest, manifest)
                exclusive_write(args.manifest_sha256_output, digest)
                print(digest.decode("ascii").strip())
            return 0

        require_full_arguments(parser, args)
        if args.mode == "generate":
            outputs = (
                args.output,
                args.notices_output,
                args.size_report_output,
                args.candidate_manifest,
            )
            if len(set(outputs)) != len(outputs):
                parser.error("generate outputs must be distinct")
            bundle = build_bundle(args)
            atomic_write(args.output, bundle["sbom"])
            atomic_write(args.notices_output, bundle["notices"])
            atomic_write(args.size_report_output, bundle["size"])
            atomic_write(args.candidate_manifest, bundle["candidate"])
            print(sha256_bytes(bundle["candidate"]))
        else:
            args.output = args.sbom
            args.notices_output = args.notices
            args.size_report_output = args.size_report
            bundle = build_bundle(args)
            actual = {
                "candidate": regular_bytes(
                    args.candidate_manifest,
                    "candidate manifest",
                    16 * 1024 * 1024,
                ),
                "notices": regular_bytes(
                    args.notices,
                    "third-party notices",
                    32 * 1024 * 1024,
                ),
                "sbom": regular_bytes(
                    args.sbom,
                    "CycloneDX SBOM",
                    16 * 1024 * 1024,
                ),
                "size": regular_bytes(
                    args.size_report,
                    "binary size report",
                    256 * 1024,
                ),
            }
            mismatched = [name for name in bundle if actual[name] != bundle[name]]
            if mismatched:
                raise ValueError(
                    "release evidence does not match the exact artifact, source, "
                    "build, license, feature, and dependency material: "
                    + ", ".join(sorted(mismatched))
                )
            print(sha256_bytes(actual["candidate"]))
    except (OSError, ValueError) as error:
        raise SystemExit(f"error: {error}") from error
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
