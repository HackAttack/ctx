#!/usr/bin/env python3
"""Build, validate, and describe the signed public Semantic release assets."""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import shutil
import stat
import sys
import tarfile
import tempfile
import urllib.request
from pathlib import Path
from typing import BinaryIO

_SCRIPT_DIRECTORY = os.fspath(Path(__file__).resolve().parent)
sys.path.insert(0, _SCRIPT_DIRECTORY)
try:
    from semantic_release_assets.common import (
        AssetError,
        canonical_json,
        download_exact_url,
        sha256_file,
        sha256_stream,
        validate_artifact_name,
        validate_lowercase_sha256,
        validate_relative_path,
        windows_reserved_component,
    )
    from semantic_release_assets.contracts import *
finally:
    del sys.path[0]
del _SCRIPT_DIRECTORY


def model_required_files(variant: str) -> dict[str, tuple[int, str]]:
    selected = MODEL_VARIANTS[variant]
    return {
        **COMMON_MODEL_FILES,
        "onnx/model.onnx": (
            selected["onnx_size"],
            selected["onnx_sha256"],
        ),
    }


def model_manifest(variant: str) -> bytes:
    return canonical_json(
        {
            "model_contract": {
                "dimensions": 384,
                "model_id": MODEL_ID,
                "normalization": "l2",
                "passage_prefix": "passage: ",
                "pooling": "attention_mask_mean",
                "query_prefix": "query: ",
                "revision": MODEL_REVISION,
            },
            "schema_version": SCHEMA_VERSION,
            "variant": variant,
            "version": MODEL_VERSION,
        }
    )


def verify_model_source(source: Path, variant: str) -> None:
    metadata = source.lstat()
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
        raise AssetError(f"model source is not a real directory: {source}")
    for relative, expected in model_required_files(variant).items():
        size, digest = sha256_file(source.joinpath(*relative.split("/")))
        if (size, digest) != expected:
            raise AssetError(
                f"pinned model file mismatch for {relative}: "
                f"expected {expected[0]}/{expected[1]}, got {size}/{digest}"
            )


def prepare_model_source(args: argparse.Namespace) -> None:
    selected = MODEL_VARIANTS[args.variant]
    if args.output_dir.exists() or args.output_dir.is_symlink():
        raise AssetError(f"model source output already exists: {args.output_dir}")
    args.output_dir.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(
        prefix=f".{args.output_dir.name}.prepare.", dir=args.output_dir.parent
    ) as temporary:
        source = Path(temporary) / "source"
        source.mkdir()
        for relative, expected in COMMON_MODEL_FILES.items():
            destination = source.joinpath(*relative.split("/"))
            url = (
                MODEL_LICENSE_URL
                if relative == "LICENSE"
                else f"{MODEL_REVISION_URL}/{relative}"
            )
            download_exact_url(
                url,
                destination,
                expected[0],
                expected[1],
            )
        onnx_expected = (selected["onnx_size"], selected["onnx_sha256"])
        download_exact_url(
            f"{MODEL_REVISION_URL}/{selected['upstream_onnx']}",
            source / "onnx" / "model.onnx",
            onnx_expected[0],
            onnx_expected[1],
        )
        verify_model_source(source, args.variant)
        os.replace(source, args.output_dir)
    print(f"source={args.output_dir}")


def add_tar_directory(bundle: tarfile.TarFile, name: str) -> None:
    entry = tarfile.TarInfo(name)
    entry.type = tarfile.DIRTYPE
    entry.mode = 0o755
    entry.uid = entry.gid = 0
    entry.uname = entry.gname = ""
    entry.mtime = 0
    bundle.addfile(entry)


def add_tar_file(bundle: tarfile.TarFile, source: Path, name: str) -> None:
    size = source.stat().st_size
    entry = tarfile.TarInfo(name)
    entry.type = tarfile.REGTYPE
    entry.mode = 0o644
    entry.uid = entry.gid = 0
    entry.uname = entry.gname = ""
    entry.mtime = 0
    entry.size = size
    with source.open("rb") as stream:
        bundle.addfile(entry, stream)


def build_model(args: argparse.Namespace) -> None:
    selected = MODEL_VARIANTS[args.variant]
    verify_model_source(args.source, args.variant)
    args.output_dir.mkdir(parents=True, exist_ok=True)
    artifact = args.output_dir / selected["artifact"]
    metadata_path = artifact.with_suffix(artifact.suffix + ".asset.json")
    checksum_path = artifact.with_suffix(artifact.suffix + ".sha256")
    for output in (artifact, metadata_path, checksum_path):
        if output.exists() or output.is_symlink():
            raise AssetError(f"refusing to replace existing output: {output}")

    prefix = artifact.name.removesuffix(".tar.xz")
    with tempfile.TemporaryDirectory(
        prefix=f".{prefix}.", dir=args.output_dir
    ) as temporary:
        staging = Path(temporary)
        for relative in MODEL_PATHS:
            destination = staging.joinpath(*relative.split("/"))
            destination.parent.mkdir(parents=True, exist_ok=True)
            if relative == "manifest.json":
                destination.write_bytes(model_manifest(args.variant))
            else:
                source = args.source.joinpath(*relative.split("/"))
                with source.open("rb") as input_stream, destination.open(
                    "xb"
                ) as output_stream:
                    while block := input_stream.read(1024 * 1024):
                        output_stream.write(block)
        temporary_archive = staging / f".{artifact.name}.tmp"
        with tarfile.open(
            temporary_archive, "w:xz", format=tarfile.USTAR_FORMAT, preset=9
        ) as bundle:
            add_tar_directory(bundle, prefix)
            add_tar_file(bundle, staging / "LICENSE", f"{prefix}/LICENSE")
            add_tar_file(bundle, staging / "config.json", f"{prefix}/config.json")
            add_tar_file(bundle, staging / "manifest.json", f"{prefix}/manifest.json")
            add_tar_directory(bundle, f"{prefix}/onnx")
            add_tar_file(
                bundle, staging / "onnx" / "model.onnx", f"{prefix}/onnx/model.onnx"
            )
            for relative in (
                "special_tokens_map.json",
                "tokenizer.json",
                "tokenizer_config.json",
            ):
                add_tar_file(bundle, staging / relative, f"{prefix}/{relative}")
        os.replace(temporary_archive, artifact)

    records = validate_model_archive(artifact, args.variant)
    write_asset_record(
        metadata_path,
        selected["asset_id"],
        "model",
        "onnx",
        MODEL_VERSION,
        "any",
        "tar.xz",
        prefix,
        artifact,
        records,
    )
    _, archive_sha256 = sha256_file(artifact)
    checksum_path.write_text(f"{archive_sha256}  {artifact.name}\n", encoding="ascii")
    print(f"artifact={artifact}")
    print(f"metadata={metadata_path}")


def canonical_tar_name(raw: str) -> str:
    if not raw or "\\" in raw or raw.startswith("/"):
        raise AssetError(f"unsafe model archive path: {raw!r}")
    directory = raw.endswith("/")
    name = raw[:-1] if directory else raw
    validate_relative_path(name)
    return name


def extract_coreml_archive(archive: Path, destination: Path) -> None:
    if destination.exists() or destination.is_symlink():
        raise AssetError(f"Core ML extraction output already exists: {destination}")
    destination.mkdir(mode=0o700)
    try:
        with tarfile.open(archive, "r:xz") as bundle:
            folded: set[str] = set()
            total = 0
            files = 0
            root_seen = False
            for index, member in enumerate(bundle):
                if index >= EXPECTED_ASSETS["apple_coreml"]["max_files"] + COREML_MAX_DIRECTORIES:
                    raise AssetError("Core ML archive contains too many entries")
                name = canonical_tar_name(member.name)
                if name != COREML_ARCHIVE_ROOT and not name.startswith(
                    f"{COREML_ARCHIVE_ROOT}/"
                ):
                    raise AssetError(f"Core ML archive path is outside its root: {name}")
                relative = (
                    "" if name == COREML_ARCHIVE_ROOT else name[len(COREML_ARCHIVE_ROOT) + 1 :]
                )
                key = relative.casefold()
                if key in folded:
                    raise AssetError(
                        f"duplicate or case-colliding Core ML archive path: {name}"
                    )
                folded.add(key)
                if not relative:
                    if not member.isdir():
                        raise AssetError("Core ML archive root is not a directory")
                    root_seen = True
                    continue
                output_path = destination.joinpath(*relative.split("/"))
                if member.isdir():
                    output_path.mkdir(parents=True, exist_ok=True, mode=0o755)
                    continue
                if not member.isfile():
                    raise AssetError(f"unsupported Core ML archive entry: {name}")
                files += 1
                total += member.size
                if (
                    member.size <= 0
                    or files > EXPECTED_ASSETS["apple_coreml"]["max_files"]
                    or total > EXPECTED_ASSETS["apple_coreml"]["max_expanded_bytes"]
                ):
                    raise AssetError("Core ML archive exceeds its extraction limits")
                source = bundle.extractfile(member)
                if source is None:
                    raise AssetError(f"could not read Core ML archive entry: {relative}")
                output_path.parent.mkdir(parents=True, exist_ok=True, mode=0o755)
                size = 0
                with source, output_path.open("xb") as output:
                    while block := source.read(1024 * 1024):
                        size += len(block)
                        if size > member.size:
                            raise AssetError(
                                f"Core ML archive entry exceeds its header: {relative}"
                            )
                        output.write(block)
                if size != member.size:
                    raise AssetError(f"truncated Core ML archive entry: {relative}")
                output_path.chmod(0o644)
            if not root_seen:
                raise AssetError("Core ML archive is missing its root directory")
    except Exception:
        shutil.rmtree(destination, ignore_errors=True)
        raise


def coreml_source_paths(source: Path) -> dict[str, Path]:
    manifest_path = source / "manifest.json"
    _, manifest_sha256 = sha256_file(manifest_path, 1024 * 1024)
    if manifest_sha256 != COREML_SOURCE_MANIFEST_SHA256:
        raise AssetError("Core ML source manifest does not match its input pin")
    manifest = json.loads(
        manifest_path.read_bytes(), object_pairs_hook=reject_duplicate_json_keys
    )
    expected_artifacts = {
        key: relative
        for key, relative in COREML_SOURCE_PATHS.items()
        if key != "model_license"
    }
    if manifest.get("artifacts") != expected_artifacts:
        raise AssetError("Core ML manifest does not name the exact prepared inputs")
    model = manifest.get("model")
    if (
        manifest.get("bundle_id") != "ctx.multilingual-e5-small.coreml.fp16"
        or manifest.get("bundle_version") != MODEL_VERSION
        or not isinstance(model, dict)
        or model.get("id") != MODEL_ID
        or model.get("source_revision") != MODEL_REVISION
    ):
        raise AssetError("Core ML manifest does not match the pinned model authority")

    paths = {
        name: source.joinpath(*relative.split("/"))
        for name, relative in COREML_SOURCE_PATHS.items()
    }
    for name in ("tokenizer", "model_license"):
        metadata = paths[name].lstat()
        if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
            raise AssetError(f"Core ML {name} is not a regular file")
    for name in ("document_model", "query_model"):
        metadata = paths[name].lstat()
        if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
            raise AssetError(f"Core ML {name} is not a real directory")
    return paths


def prepare_coreml_source(args: argparse.Namespace) -> None:
    if args.output_dir.exists() or args.output_dir.is_symlink():
        raise AssetError(f"Core ML source output already exists: {args.output_dir}")
    args.output_dir.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(
        prefix=f".{args.output_dir.name}.prepare.", dir=args.output_dir.parent
    ) as temporary:
        staging = Path(temporary)
        archive = staging / COREML_ARCHIVE_NAME
        download_exact_url(
            COREML_SOURCE_ARCHIVE_URL,
            archive,
            COREML_SOURCE_ARCHIVE_SIZE,
            COREML_SOURCE_ARCHIVE_SHA256,
        )
        source = staging / "source"
        extract_coreml_archive(archive, source)
        coreml_source_paths(source)
        os.replace(source, args.output_dir)
    print(f"source={args.output_dir}")


def validate_model_archive(archive: Path, variant: str) -> list[dict[str, object]]:
    selected = MODEL_VARIANTS[variant]
    if archive.name != selected["artifact"]:
        raise AssetError(
            f"model archive must be named {selected['artifact']}, got {archive.name}"
        )
    prefix = archive.name.removesuffix(".tar.xz")
    expected_files = {f"{prefix}/{path}": path for path in MODEL_PATHS}
    expected_directories = {prefix, f"{prefix}/onnx"}
    seen: set[str] = set()
    records = []
    total = 0
    with tarfile.open(archive, "r:xz") as bundle:
        for member in bundle:
            name = canonical_tar_name(member.name)
            folded = name.casefold()
            if folded in seen:
                raise AssetError(f"duplicate or case-colliding archive path: {name}")
            seen.add(folded)
            if member.mode & 0o7000:
                raise AssetError(f"unsafe mode on model archive path: {name}")
            if member.isdir():
                if name not in expected_directories:
                    raise AssetError(f"unexpected model archive directory: {name}")
                continue
            relative = expected_files.get(name)
            if relative is None or not member.isfile():
                raise AssetError(f"unexpected model archive entry: {name}")
            total += member.size
            if member.size <= 0 or total > MODEL_MAX_EXPANDED_BYTES:
                raise AssetError("model archive exceeds its expanded-size limit")
            source = bundle.extractfile(member)
            if source is None:
                raise AssetError(f"could not read model archive entry: {name}")
            with source:
                size, digest = sha256_stream(source, member.size)
            if size != member.size:
                raise AssetError(f"truncated model archive entry: {name}")
            records.append({"path": relative, "sha256": digest, "size": size})
    if seen != {
        *(name.casefold() for name in expected_files),
        *(name.casefold() for name in expected_directories),
    }:
        raise AssetError("model archive does not contain the exact required path set")
    records.sort(key=lambda value: str(value["path"]))
    record_map = {
        str(record["path"]): (record["size"], record["sha256"]) for record in records
    }
    for relative, expected in model_required_files(variant).items():
        if record_map.get(relative) != expected:
            raise AssetError(f"pinned model identity mismatch in archive: {relative}")
    expected_manifest = model_manifest(variant)
    manifest_record = record_map["manifest.json"]
    if manifest_record != (
        len(expected_manifest),
        hashlib.sha256(expected_manifest).hexdigest(),
    ):
        raise AssetError("model archive manifest is not canonical")
    return records


def validate_model(args: argparse.Namespace) -> None:
    records = validate_model_archive(args.archive, args.variant)
    _, digest = sha256_file(args.archive)
    print(f"archive_sha256={digest}")
    print(f"files={len(records)}")


def collect_records(root: Path, paths: list[str]) -> list[dict[str, object]]:
    if paths != sorted(set(paths)):
        raise AssetError("--file values must be unique and sorted")
    records = []
    folded: set[str] = set()
    for relative in paths:
        validate_relative_path(relative)
        if relative.casefold() in folded:
            raise AssetError(f"case-colliding asset path: {relative}")
        folded.add(relative.casefold())
        size, digest = sha256_file(root.joinpath(*relative.split("/")))
        if size == 0:
            raise AssetError(f"asset file must not be empty: {relative}")
        records.append({"path": relative, "sha256": digest, "size": size})
    actual = []
    for directory, names, files in os.walk(root, followlinks=False):
        names.sort()
        files.sort()
        current = Path(directory)
        for name in names:
            metadata = (current / name).lstat()
            if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
                raise AssetError(f"unsupported asset directory: {current / name}")
        for name in files:
            path = current / name
            metadata = path.lstat()
            if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
                raise AssetError(f"unsupported asset file: {path}")
            actual.append(path.relative_to(root).as_posix())
    if sorted(actual) != paths:
        raise AssetError("staged asset files do not exactly match --file values")
    return records


ASSET_FIELDS = {
    "archive_format",
    "archive_path_prefix",
    "archive_sha256",
    "artifact",
    "backend",
    "files",
    "max_expanded_bytes",
    "max_files",
    "platform",
    "role",
    "version",
}
FILE_FIELDS = {"path", "sha256", "size"}
COREML_EXACT_PATHS = {
    "PROVENANCE.json",
    "THIRD_PARTY_NOTICES.md",
    "manifest.json",
    "tokenizer.json",
}
COREML_TREE_PREFIXES = ("LICENSES/", "document.mlpackage/", "query.mlpackage/")


def validate_asset_record(asset_id: str, asset: object) -> None:
    if asset_id not in EXPECTED_ASSETS:
        raise AssetError(f"unsupported Semantic asset ID: {asset_id!r}")
    if not isinstance(asset, dict) or set(asset) != ASSET_FIELDS:
        raise AssetError(f"invalid Semantic asset fields for {asset_id}")
    expected = EXPECTED_ASSETS[asset_id]
    for field in (
        "role",
        "backend",
        "version",
        "platform",
        "artifact",
        "archive_format",
        "archive_path_prefix",
        "max_expanded_bytes",
        "max_files",
    ):
        if asset[field] != expected[field] or type(asset[field]) is not type(expected[field]):
            raise AssetError(f"noncanonical {field} for Semantic asset {asset_id}")

    validate_artifact_name(asset["artifact"])
    validate_lowercase_sha256(asset["archive_sha256"])
    prefix = asset["archive_path_prefix"]
    if prefix:
        validate_relative_path(prefix)
    files = asset["files"]
    if not isinstance(files, list) or not files or len(files) > expected["max_files"]:
        raise AssetError(f"unsafe file count for Semantic asset {asset_id}")

    paths = []
    folded: set[str] = set()
    total = 0
    records: dict[str, tuple[int, str]] = {}
    previous = None
    for record in files:
        if not isinstance(record, dict) or set(record) != FILE_FIELDS:
            raise AssetError(f"invalid file record for Semantic asset {asset_id}")
        path = record["path"]
        size = record["size"]
        digest = record["sha256"]
        if not isinstance(path, str):
            raise AssetError(f"invalid file path for Semantic asset {asset_id}")
        validate_relative_path(path)
        if previous is not None and previous >= path:
            raise AssetError(f"file records are not strictly sorted for {asset_id}")
        previous = path
        if path.casefold() in folded:
            raise AssetError(f"duplicate or case-colliding file path for {asset_id}")
        folded.add(path.casefold())
        if (
            (asset["backend"].startswith("ort-") or asset["backend"] == "windows-ml")
            and path == "ctx-runtime-install.json"
        ):
            raise AssetError(f"{asset_id} claims the reserved install manifest path")
        if type(size) is not int or size <= 0:
            raise AssetError(f"invalid file size for Semantic asset {asset_id}")
        validate_lowercase_sha256(digest)
        total += size
        if total > expected["max_expanded_bytes"]:
            raise AssetError(f"expanded size exceeds signed limit for {asset_id}")
        paths.append(path)
        records[path] = (size, digest)

    expected_paths = expected["files"]
    if expected_paths is not None:
        if paths != sorted(expected_paths):
            raise AssetError(f"wrong file inventory for Semantic asset {asset_id}")
    else:
        path_set = set(paths)
        missing = COREML_EXACT_PATHS - path_set
        missing_prefixes = [
            prefix for prefix in COREML_TREE_PREFIXES if not any(path.startswith(prefix) for path in paths)
        ]
        if missing or missing_prefixes:
            raise AssetError(
                f"Core ML asset is missing required paths: "
                f"{sorted(missing) + missing_prefixes}"
            )
        if any(
            path not in COREML_EXACT_PATHS
            and not path.startswith(COREML_TREE_PREFIXES)
            for path in paths
        ):
            raise AssetError("Core ML asset contains an unexpected path")

    if asset_id in ("onnx_model", "onnx_model_o4_fp16"):
        variant = (
            "cpu-fp32" if asset_id == "onnx_model" else "accelerator-o4-fp16"
        )
        for path, pinned in model_required_files(variant).items():
            if records.get(path) != pinned:
                raise AssetError(f"pinned model identity mismatch for {asset_id}: {path}")
        if records["LICENSE"][0] <= 0:
            raise AssetError(f"model LICENSE must not be empty for {asset_id}")
        manifest = model_manifest(variant)
        if records["manifest.json"] != (
            len(manifest),
            hashlib.sha256(manifest).hexdigest(),
        ):
            raise AssetError(f"model manifest is not canonical for {asset_id}")
    elif asset_id == "apple_coreml":
        if asset["archive_sha256"] != COREML_PUBLICATION_ARCHIVE_SHA256:
            raise AssetError("Core ML archive does not match its publication pin")
        if records["manifest.json"][1] != COREML_PUBLICATION_MANIFEST_SHA256:
            raise AssetError("Core ML manifest does not match its publication pin")


def asset_record(
    asset_id: str,
    role: str,
    backend: str,
    version: str,
    platform: str,
    archive_format: str,
    prefix: str,
    artifact: Path,
    records: list[dict[str, object]],
) -> dict[str, object]:
    expected = EXPECTED_ASSETS.get(asset_id)
    if expected is None:
        raise AssetError(f"unsupported Semantic asset ID: {asset_id!r}")
    supplied = {
        "role": role,
        "backend": backend,
        "version": version,
        "platform": platform,
        "archive_format": archive_format,
        "archive_path_prefix": prefix,
        "artifact": artifact.name,
    }
    for field, value in supplied.items():
        if value != expected[field]:
            raise AssetError(f"noncanonical {field} for Semantic asset {asset_id}")
    archive_size, archive_sha256 = sha256_file(artifact)
    if asset_id == "apple_coreml" and archive_size != COREML_PUBLICATION_ARCHIVE_SIZE:
        raise AssetError("Core ML archive size does not match its publication pin")
    value = {
        "id": asset_id,
        "asset": {
            "archive_format": archive_format,
            "archive_path_prefix": prefix,
            "archive_sha256": archive_sha256,
            "artifact": artifact.name,
            "backend": backend,
            "files": records,
            "max_expanded_bytes": expected["max_expanded_bytes"],
            "max_files": expected["max_files"],
            "platform": platform,
            "role": role,
            "version": version,
        },
    }
    validate_asset_record(asset_id, value["asset"])
    return value


def write_asset_record(
    output: Path,
    asset_id: str,
    role: str,
    backend: str,
    version: str,
    platform: str,
    archive_format: str,
    prefix: str,
    artifact: Path,
    records: list[dict[str, object]],
) -> None:
    output.write_bytes(
        canonical_json(
            asset_record(
                asset_id,
                role,
                backend,
                version,
                platform,
                archive_format,
                prefix,
                artifact,
                records,
            )
        )
        + b"\n"
    )


def record_asset(args: argparse.Namespace) -> None:
    records = collect_records(args.root, args.file)
    write_asset_record(
        args.output,
        args.asset_id,
        args.role,
        args.backend,
        args.version,
        args.platform,
        args.archive_format,
        args.archive_path_prefix,
        args.archive,
        records,
    )
    print(f"metadata={args.output}")


def authority(target: str, backend: str, asset_ids: list[str]) -> dict[str, object]:
    return {
        "asset_ids": asset_ids,
        "backend": backend,
        "model_contract": {
            "dimensions": 384,
            "model_id": MODEL_ID,
            "normalization": "l2",
            "passage_prefix": "passage: ",
            "pooling": "attention_mask_mean",
            "query_prefix": "query: ",
            "revision": MODEL_REVISION,
        },
        "runtime_install_manifest_schema_version": 1,
        "schema_version": 1,
        "target": target,
    }


def encode_record(value: object) -> str:
    return base64.b64encode(canonical_json(value)).decode("ascii")


def reject_duplicate_json_keys(pairs: list[tuple[str, object]]) -> dict[str, object]:
    value: dict[str, object] = {}
    for key, item in pairs:
        if key in value:
            raise AssetError(f"duplicate JSON key in Semantic asset record: {key}")
        value[key] = item
    return value


def bind_coreml_cache(args: argparse.Namespace) -> None:
    archive = args.archive
    if archive.name != COREML_ARCHIVE_NAME:
        raise AssetError(
            f"Core ML archive must be named {COREML_ARCHIVE_NAME}, got {archive.name}"
    )
    checksum_path = Path(f"{archive}.sha256")
    asset_record_path = Path(f"{archive}.asset.json")
    checksum_metadata = checksum_path.lstat()
    if (
        stat.S_ISLNK(checksum_metadata.st_mode)
        or not stat.S_ISREG(checksum_metadata.st_mode)
        or checksum_metadata.st_size > 256
    ):
        raise AssetError("Core ML checksum sidecar is not a bounded regular file")
    checksum = checksum_path.read_bytes()
    expected_checksum = (
        f"{COREML_PUBLICATION_ARCHIVE_SHA256}  {COREML_ARCHIVE_NAME}\n".encode(
            "ascii"
        )
    )
    if checksum != expected_checksum:
        raise AssetError("Core ML checksum sidecar does not match its publication pin")

    archive_size, archive_sha256 = sha256_file(archive)
    if (archive_size, archive_sha256) != (
        COREML_PUBLICATION_ARCHIVE_SIZE,
        COREML_PUBLICATION_ARCHIVE_SHA256,
    ):
        raise AssetError(
            "Core ML archive does not match its publication size/SHA-256 pin"
        )

    record_metadata = asset_record_path.lstat()
    if (
        stat.S_ISLNK(record_metadata.st_mode)
        or not stat.S_ISREG(record_metadata.st_mode)
        or record_metadata.st_size > 16 * 1024 * 1024
    ):
        raise AssetError("Core ML asset record is not a bounded regular file")
    record_raw = asset_record_path.read_bytes()
    record = json.loads(record_raw, object_pairs_hook=reject_duplicate_json_keys)
    if (
        not isinstance(record, dict)
        or set(record) != {"asset", "id"}
        or record.get("id") != "apple_coreml"
        or canonical_json(record) + b"\n" != record_raw
    ):
        raise AssetError("Core ML asset record is not canonical apple_coreml metadata")
    asset = record["asset"]
    validate_asset_record("apple_coreml", asset)
    if asset["archive_sha256"] != archive_sha256:
        raise AssetError("Core ML asset record does not match the candidate archive")

    cache_metadata = args.cache_dir.lstat()
    if stat.S_ISLNK(cache_metadata.st_mode) or not stat.S_ISDIR(
        cache_metadata.st_mode
    ):
        raise AssetError("Core ML cache root is not a real directory")
    if stat.S_IMODE(cache_metadata.st_mode) & 0o077:
        raise AssetError("Core ML cache root is not owner-private")
    if next(args.cache_dir.iterdir(), None) is not None:
        raise AssetError("Core ML candidate binding requires an empty cache root")

    manifest_sha256 = COREML_PUBLICATION_MANIFEST_SHA256
    target_parent = (
        args.cache_dir
        / "semantic-model-bundles"
        / "sha256"
        / manifest_sha256[:2]
    )
    target = target_parent / manifest_sha256
    marker = target.with_name(f"{manifest_sha256}.complete.json")
    installed = False
    try:
        with tempfile.TemporaryDirectory(
            prefix=".candidate-coreml.", dir=args.cache_dir
        ) as temporary:
            staging = Path(temporary) / "bundle"
            extract_coreml_archive(archive, staging)
            paths = [entry["path"] for entry in asset["files"]]
            if collect_records(staging, paths) != asset["files"]:
                raise AssetError(
                    "Core ML candidate contents do not match the canonical asset record"
                )
            _, actual_manifest_sha256 = sha256_file(
                staging / "manifest.json", 1024 * 1024
            )
            if actual_manifest_sha256 != manifest_sha256:
                raise AssetError(
                    "Core ML candidate manifest does not match its publication pin"
                )
            target_parent.mkdir(parents=True, mode=0o700)
            os.replace(staging, target)
            installed = True
        marker_body = canonical_json(
            {"manifest_sha256": manifest_sha256, "schema_version": 1}
        ) + b"\n"
        with marker.open("xb") as output:
            output.write(marker_body)
            output.flush()
            os.fsync(output.fileno())
    except Exception:
        marker.unlink(missing_ok=True)
        if installed:
            shutil.rmtree(target, ignore_errors=True)
        raise

    print(f"archive_sha256={archive_sha256}")
    print(f"manifest_sha256={manifest_sha256}")
    print(f"cache_bundle={target}")


def build_catalog(args: argparse.Namespace) -> None:
    records: dict[str, dict[str, object]] = {}
    for path in args.asset_record:
        raw = path.read_bytes()
        value = json.loads(raw, object_pairs_hook=reject_duplicate_json_keys)
        if set(value) != {"asset", "id"} or not isinstance(value["id"], str):
            raise AssetError(f"invalid Semantic asset record: {path}")
        asset_id = value["id"]
        if asset_id in records:
            raise AssetError(f"duplicate Semantic asset record: {asset_id}")
        if canonical_json(value) + b"\n" != raw:
            raise AssetError(f"Semantic asset record is not canonical JSON: {path}")
        validate_asset_record(asset_id, value["asset"])
        records[asset_id] = value["asset"]
    if set(records) != EXPECTED_ASSET_IDS:
        missing = sorted(EXPECTED_ASSET_IDS - set(records))
        extra = sorted(set(records) - EXPECTED_ASSET_IDS)
        raise AssetError(f"wrong Semantic asset set; missing={missing}, extra={extra}")

    values = {
        "CTX_RELEASE_SEMANTIC_SCHEMA_VERSION": "1",
        "CTX_RELEASE_SEMANTIC_ASSETS": encode_record(
            {"assets": records, "schema_version": 1}
        ),
        "CTX_RELEASE_SEMANTIC_AUTHORITY_apple_silicon_coreml": encode_record(
            authority(
                "apple-silicon",
                "coreml",
                ["onnx_model", "macos_arm64_cpu", "apple_coreml"],
            )
        ),
        "CTX_RELEASE_SEMANTIC_AUTHORITY_windows_windows_ml": encode_record(
            authority(
                "windows",
                "windows-ml",
                ["onnx_model_o4_fp16", "windows_ml"],
            )
        ),
        "CTX_RELEASE_SEMANTIC_AUTHORITY_linux_nvidia_ort_cuda": encode_record(
            authority(
                "linux-nvidia",
                "ort-cuda",
                ["onnx_model_o4_fp16", "linux_cuda12"],
            )
        ),
        "CTX_RELEASE_SEMANTIC_AUTHORITY_universal_ort_cpu": encode_record(
            authority(
                "universal",
                "ort-cpu",
                [
                    "onnx_model",
                    "linux_x64_cpu",
                    "linux_aarch64_cpu",
                    "macos_arm64_cpu",
                    "macos_x64_cpu",
                    "windows_ml",
                ],
            )
        ),
    }
    args.output.write_text(
        "".join(f"{key}={value}\n" for key, value in values.items()),
        encoding="ascii",
    )
    print(f"metadata={args.output}")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)

    prepare_model = commands.add_parser("prepare-model")
    prepare_model.add_argument(
        "--variant", choices=tuple(MODEL_VARIANTS), required=True
    )
    prepare_model.add_argument("--output-dir", type=Path, required=True)
    prepare_model.set_defaults(run=prepare_model_source)

    prepare_coreml = commands.add_parser("prepare-coreml")
    prepare_coreml.add_argument("--output-dir", type=Path, required=True)
    prepare_coreml.set_defaults(run=prepare_coreml_source)

    build = commands.add_parser("build-model")
    build.add_argument("--variant", choices=tuple(MODEL_VARIANTS), required=True)
    build.add_argument("--source", type=Path, required=True)
    build.add_argument("--output-dir", type=Path, required=True)
    build.set_defaults(run=build_model)

    validate = commands.add_parser("validate-model")
    validate.add_argument("--variant", choices=tuple(MODEL_VARIANTS), required=True)
    validate.add_argument("--archive", type=Path, required=True)
    validate.set_defaults(run=validate_model)

    record = commands.add_parser("record")
    record.add_argument("--asset-id", required=True)
    record.add_argument("--role", required=True)
    record.add_argument("--backend", required=True)
    record.add_argument("--version", required=True)
    record.add_argument("--platform", required=True)
    record.add_argument("--archive-format", choices=("tar.xz", "tar.zst", "zip"), required=True)
    record.add_argument("--archive-path-prefix", default="")
    record.add_argument("--archive", type=Path, required=True)
    record.add_argument("--root", type=Path, required=True)
    record.add_argument("--file", action="append", default=[], required=True)
    record.add_argument("--output", type=Path, required=True)
    record.set_defaults(run=record_asset)

    catalog = commands.add_parser("catalog")
    catalog.add_argument("--asset-record", action="append", type=Path, required=True)
    catalog.add_argument("--output", type=Path, required=True)
    catalog.set_defaults(run=build_catalog)

    bind_coreml = commands.add_parser("bind-coreml-cache")
    bind_coreml.add_argument("--archive", type=Path, required=True)
    bind_coreml.add_argument("--cache-dir", type=Path, required=True)
    bind_coreml.set_defaults(run=bind_coreml_cache)
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    try:
        args.run(args)
    except (
        AssetError,
        OSError,
        tarfile.TarError,
        json.JSONDecodeError,
    ) as error:
        raise SystemExit(f"error: {error}") from error


if __name__ == "__main__":
    main()
