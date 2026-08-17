"""File-safety and canonical encoding primitives for Semantic assets."""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import stat
from typing import BinaryIO
import urllib.request

from .contracts import DOWNLOAD_USER_AGENT


class AssetError(ValueError):
    pass


def canonical_json(value: object) -> bytes:
    return json.dumps(
        value, ensure_ascii=True, separators=(",", ":"), sort_keys=True
    ).encode("ascii")


def sha256_stream(stream: BinaryIO, maximum: int) -> tuple[int, str]:
    size = 0
    digest = hashlib.sha256()
    while block := stream.read(1024 * 1024):
        size += len(block)
        if size > maximum:
            raise AssetError(f"file exceeds {maximum} byte safety limit")
        digest.update(block)
    return size, digest.hexdigest()


def sha256_file(path: Path, maximum: int = 2 * 1024 * 1024 * 1024) -> tuple[int, str]:
    flags = os.O_RDONLY
    if hasattr(os, "O_CLOEXEC"):
        flags |= os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    descriptor = os.open(path, flags)
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode):
            raise AssetError(f"not a regular file: {path}")
        with os.fdopen(descriptor, "rb", closefd=False) as stream:
            size, digest = sha256_stream(stream, maximum)
        after = path.lstat()
        if (
            stat.S_ISLNK(after.st_mode)
            or (before.st_dev, before.st_ino) != (after.st_dev, after.st_ino)
        ):
            raise AssetError(f"file changed while hashing: {path}")
        return size, digest
    finally:
        os.close(descriptor)


def download_exact_url(
    url: str, destination: Path, expected_size: int, expected_sha256: str
) -> None:
    if destination.exists() or destination.is_symlink():
        raise AssetError(f"refusing to replace existing download: {destination}")
    destination.parent.mkdir(parents=True, exist_ok=True)
    digest = hashlib.sha256()
    size = 0
    try:
        request = urllib.request.Request(
            url,
            headers={"User-Agent": DOWNLOAD_USER_AGENT},
            method="GET",
        )
        with urllib.request.urlopen(request, timeout=300) as response, destination.open(
            "xb"
        ) as output:
            while block := response.read(1024 * 1024):
                size += len(block)
                if size > expected_size:
                    raise AssetError(
                        f"download exceeds pinned size for {url}: {size} > {expected_size}"
                    )
                digest.update(block)
                output.write(block)
    except Exception:
        destination.unlink(missing_ok=True)
        raise
    actual_sha256 = digest.hexdigest()
    if (size, actual_sha256) != (expected_size, expected_sha256):
        destination.unlink(missing_ok=True)
        raise AssetError(
            f"pinned download mismatch for {url}: expected "
            f"{expected_size}/{expected_sha256}, got {size}/{actual_sha256}"
        )


def validate_relative_path(value: str) -> None:
    if (
        not value
        or not value.isascii()
        or any(not 0x20 <= byte <= 0x7E for byte in value.encode("ascii"))
        or "\\" in value
        or ":" in value
        or value.startswith("/")
        or value.endswith("/")
        or "//" in value
        or len(value.encode("ascii")) > 512
    ):
        raise AssetError(f"unsafe asset path: {value!r}")
    if any(
        part in ("", ".", "..")
        or part.endswith(".")
        or part.endswith(" ")
        or windows_reserved_component(part)
        for part in value.split("/")
    ):
        raise AssetError(f"unsafe asset path: {value!r}")


def windows_reserved_component(component: str) -> bool:
    stem = component.split(".", 1)[0].upper()
    return stem in {"CON", "PRN", "AUX", "NUL"} or (
        len(stem) == 4
        and stem[:3] in {"COM", "LPT"}
        and stem[3] in "123456789"
    )


def validate_lowercase_sha256(value: object) -> None:
    if (
        not isinstance(value, str)
        or len(value) != 64
        or any(byte not in b"0123456789abcdef" for byte in value.encode("ascii", "ignore"))
        or not value.isascii()
        or value == "0" * 64
    ):
        raise AssetError("Semantic checksum must use lowercase SHA-256 hex")


def validate_artifact_name(value: object) -> None:
    if not isinstance(value, str):
        raise AssetError("Semantic artifact name must be a string")
    validate_relative_path(value)
    if "/" in value or value in (".", ".."):
        raise AssetError(f"unsafe Semantic artifact name: {value!r}")
