#!/usr/bin/env python3
"""Materialize and verify the one approved macOS x64 LLVM inspector closure."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import stat
import sys
from typing import Callable, Iterable


class AuthorityError(ValueError):
    """The declared authority is unavailable or does not match policy."""


@dataclass(frozen=True)
class Member:
    source: str
    snapshot: str
    source_sha256: str
    snapshot_sha256: str
    mode: int
    replacements: tuple[tuple[bytes, bytes], ...] = ()
    candidate_neutral_audit: bool = False


@dataclass(frozen=True)
class Policy:
    authority: str
    bottle_sha256: str
    version: str
    members: tuple[Member, ...]


APPROVED_POLICY = Policy(
    authority="homebrew-core/llvm 22.1.8 sonoma x86_64 bottle",
    bottle_sha256="2f07536754d0854565f9ac37436681bb3d04a4fbb15c45c51896933262df5e48",
    version="22.1.8",
    members=(
        Member(
            "BOTTLE-AUDIT.json",
            "provenance/BOTTLE-AUDIT.json",
            "b3478d45bf5492e1e8afb30a8faa22c75697aefde3b74b8489003b4cc5d2c5cb",
            "c4ff81bf0a6de7176461d3c29a0ce0fd6fe10c0bde69711daa60e8ed4e1cbef8",
            0o400,
            candidate_neutral_audit=True,
        ),
        Member(
            "llvm.formula.json",
            "provenance/llvm.formula.json",
            "c0995cbb1a29b6daacbd09e9c1f57b00441b49737839cc8e07e72014a5f4d2ca",
            "c0995cbb1a29b6daacbd09e9c1f57b00441b49737839cc8e07e72014a5f4d2ca",
            0o400,
        ),
        Member(
            "z3.formula.json",
            "provenance/z3.formula.json",
            "71d871ff75872ac95960f01f8b8d8ac5785d9eaaff92dd9ce3f2f44d4f05bbf5",
            "71d871ff75872ac95960f01f8b8d8ac5785d9eaaff92dd9ce3f2f44d4f05bbf5",
            0o400,
        ),
        Member(
            "zstd.formula.json",
            "provenance/zstd.formula.json",
            "293691dc256cccd876379a82e11e6425ef7a9d6ab21b9e32a84d157dcea37faa",
            "293691dc256cccd876379a82e11e6425ef7a9d6ab21b9e32a84d157dcea37faa",
            0o400,
        ),
        Member(
            "bottle-members/llvm/22.1.8/bin/llvm-readobj",
            "bin/llvm-readobj",
            "48fb9e586252d630b18df7075dd1a79380d76917e41a8a76d982a71e191d7d30",
            "48fb9e586252d630b18df7075dd1a79380d76917e41a8a76d982a71e191d7d30",
            0o500,
        ),
        Member(
            "bottle-members/llvm/22.1.8/bin/llvm-objdump",
            "bin/llvm-objdump",
            "0e59712106328915251a3e26f1ac3d42da4d38debfa2b59c63eb1a9de206724d",
            "0e59712106328915251a3e26f1ac3d42da4d38debfa2b59c63eb1a9de206724d",
            0o500,
        ),
        Member(
            "bottle-members/llvm/22.1.8/lib/libLLVM.dylib",
            "lib/libLLVM.dylib",
            "03c5c78a8f4ed6d8aa8a80dbdb6cff29a53c9c611111e0186d68a8844e816437",
            "a85c279a0259379cb56c75738d9667938c89caa74a5437e169e4cbc34f8e18d2",
            0o400,
            (
                (
                    b"@@HOMEBREW_PREFIX@@/opt/llvm/lib/libLLVM.dylib",
                    b"@loader_path/libLLVM.dylib",
                ),
                (
                    b"@@HOMEBREW_PREFIX@@/opt/z3/lib/libz3.4.16.dylib",
                    b"@loader_path/libz3.4.16.dylib",
                ),
                (
                    b"@@HOMEBREW_PREFIX@@/opt/zstd/lib/libzstd.1.dylib",
                    b"@loader_path/libzstd.1.dylib",
                ),
            ),
        ),
        Member(
            "bottle-members/z3/4.16.0/lib/libz3.4.16.0.0.dylib",
            "lib/libz3.4.16.dylib",
            "e09b1741045e962fc3d4a12cde17cadb10d49149d54ff8f5cefdecea98cbbaad",
            "a915cddd56396e26ff9b51bdd7d1aa1772c87399c237f948cffea433bea84a06",
            0o400,
            (
                (
                    b"@@HOMEBREW_PREFIX@@/opt/z3/lib/libz3.4.16.dylib",
                    b"@loader_path/libz3.4.16.dylib",
                ),
            ),
        ),
        Member(
            "bottle-members/zstd/1.5.7_1/lib/libzstd.1.5.7.dylib",
            "lib/libzstd.1.dylib",
            "5c36668a2b042150303d4834eef77c2e717d19d16af7ab75e23c6f32bc1751c4",
            "168bda75f183e59790090da5f6ac7980cc78c874308b90064eebce8ca624239b",
            0o400,
            (
                (
                    b"@@HOMEBREW_PREFIX@@/opt/zstd/lib/libzstd.1.dylib",
                    b"@loader_path/libzstd.1.dylib",
                ),
            ),
        ),
    ),
)


def _identity(value: os.stat_result) -> tuple[int, int, int, int, int]:
    return (
        value.st_dev,
        value.st_ino,
        value.st_size,
        value.st_mtime_ns,
        value.st_ctime_ns,
    )


def _strict_absolute(value: str, *, name: str) -> Path:
    if not value or not os.path.isabs(value):
        raise AuthorityError(f"{name} must be absolute")
    if any(part in (".", "..") for part in value.split(os.sep)):
        raise AuthorityError(f"{name} must not contain traversal components")
    if os.path.normpath(value) != value:
        raise AuthorityError(f"{name} must be lexically normalized")
    return Path(value)


def _reject_symlink_ancestors(path: Path, *, include_leaf: bool) -> None:
    current = Path(path.anchor)
    parts = path.parts[1:] if path.is_absolute() else path.parts
    limit = len(parts) if include_leaf else max(0, len(parts) - 1)
    for part in parts[:limit]:
        current /= part
        try:
            mode = os.lstat(current).st_mode
        except OSError as error:
            raise AuthorityError(f"authority path is unavailable: {current}") from error
        if stat.S_ISLNK(mode):
            raise AuthorityError(f"authority path has a symlink ancestor: {current}")
        if not stat.S_ISDIR(mode):
            raise AuthorityError(f"authority path ancestor is not a directory: {current}")


def _validate_relative(value: str) -> PurePosixPath:
    path = PurePosixPath(value)
    if path.is_absolute() or any(part in ("", ".", "..") for part in path.parts):
        raise AuthorityError(f"invalid policy member path: {value}")
    return path


def _open_absolute_directory(path: Path) -> int:
    directory_fd = os.open(
        path.anchor,
        os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
    )
    try:
        for part in path.parts[1:]:
            next_fd = os.open(
                part,
                os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                dir_fd=directory_fd,
            )
            os.close(directory_fd)
            directory_fd = next_fd
        return directory_fd
    except Exception:
        os.close(directory_fd)
        raise


def _open_directory_at(root_fd: int, relative: str, *, create: bool = False) -> int:
    path = _validate_relative(relative)
    directory_fd = os.dup(root_fd)
    try:
        for part in path.parts:
            if create:
                try:
                    os.mkdir(part, 0o700, dir_fd=directory_fd)
                except FileExistsError:
                    pass
            next_fd = os.open(
                part,
                os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                dir_fd=directory_fd,
            )
            os.close(directory_fd)
            directory_fd = next_fd
        return directory_fd
    except OSError as error:
        os.close(directory_fd)
        raise AuthorityError(
            f"snapshot directory is unavailable without symlinks: {relative}"
        ) from error
    except Exception:
        os.close(directory_fd)
        raise


def _open_regular_at(
    root_fd: int,
    relative: str,
    *,
    flags: int = os.O_RDONLY,
) -> tuple[int, int, str]:
    path = _validate_relative(relative)
    directory_fd = os.dup(root_fd)
    try:
        for part in path.parts[:-1]:
            next_fd = os.open(
                part,
                os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                dir_fd=directory_fd,
            )
            os.close(directory_fd)
            directory_fd = next_fd
        leaf = path.parts[-1]
        file_fd = os.open(leaf, flags | os.O_NOFOLLOW, dir_fd=directory_fd)
        mode = os.fstat(file_fd).st_mode
        if not stat.S_ISREG(mode):
            os.close(file_fd)
            raise AuthorityError(f"authority member is not a regular file: {relative}")
        return file_fd, directory_fd, leaf
    except OSError as error:
        os.close(directory_fd)
        raise AuthorityError(
            f"authority member is unavailable without symlinks: {relative}"
        ) from error
    except Exception:
        os.close(directory_fd)
        raise


def _stable_fd_digest(file_fd: int, directory_fd: int, leaf: str) -> str:
    before = os.fstat(file_fd)
    digest = hashlib.sha256()
    os.lseek(file_fd, 0, os.SEEK_SET)
    while chunk := os.read(file_fd, 1024 * 1024):
        digest.update(chunk)
    after = os.fstat(file_fd)
    current = os.stat(leaf, dir_fd=directory_fd, follow_symlinks=False)
    if _identity(before) != _identity(after) or _identity(after) != _identity(current):
        raise AuthorityError(f"authority member changed while open: {leaf}")
    return digest.hexdigest()


def _copy_member(root_fd: int, snapshot_fd: int, member: Member) -> None:
    source_fd, source_parent_fd, source_leaf = _open_regular_at(root_fd, member.source)
    destination = _validate_relative(member.snapshot)
    if str(destination.parent) == ".":
        destination_parent_fd = os.dup(snapshot_fd)
    else:
        destination_parent_fd = _open_directory_at(
            snapshot_fd,
            str(destination.parent),
            create=True,
        )
    output_fd = -1
    try:
        output_fd = os.open(
            destination.name,
            os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
            0o600,
            dir_fd=destination_parent_fd,
        )
        source_before = os.fstat(source_fd)
        digest = hashlib.sha256()
        while chunk := os.read(source_fd, 1024 * 1024):
            digest.update(chunk)
            view = memoryview(chunk)
            while view:
                written = os.write(output_fd, view)
                view = view[written:]
        os.fsync(output_fd)
        source_after = os.fstat(source_fd)
        current = os.stat(source_leaf, dir_fd=source_parent_fd, follow_symlinks=False)
        if (
            _identity(source_before) != _identity(source_after)
            or _identity(source_after) != _identity(current)
        ):
            raise AuthorityError(f"authority member changed while copying: {member.source}")
        if digest.hexdigest() != member.source_sha256:
            raise AuthorityError(f"authority member digest is not approved: {member.source}")
        if (
            _stable_fd_digest(output_fd, destination_parent_fd, destination.name)
            != member.source_sha256
        ):
            raise AuthorityError(f"snapshot member changed while copying: {member.snapshot}")
    finally:
        if output_fd >= 0:
            os.close(output_fd)
        os.close(destination_parent_fd)
        os.close(source_fd)
        os.close(source_parent_fd)


def _candidate_neutral_bottle_audit(data: bytes) -> bytes:
    try:
        value = json.loads(data)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise AuthorityError("bottle audit is not valid JSON") from error
    if not isinstance(value, dict) or value.get("schema_version") != 1:
        raise AuthorityError("bottle audit schema is not approved")
    candidate = value.get("candidate")
    required_candidate_fields = {
        "target",
        "public_commit",
        "private_commit",
        "archive_name",
        "archive_size_bytes",
        "archive_sha256",
    }
    if not isinstance(candidate, dict) or set(candidate) != required_candidate_fields:
        raise AuthorityError("bottle audit candidate binding is malformed")
    if candidate["target"] != "macos-x64":
        raise AuthorityError("bottle audit candidate target is not macos-x64")
    for name in ("public_commit", "private_commit"):
        commit = candidate[name]
        if (
            not isinstance(commit, str)
            or len(commit) != 40
            or any(character not in "0123456789abcdef" for character in commit)
        ):
            raise AuthorityError(f"bottle audit {name} is not a commit")
    archive_name = candidate["archive_name"]
    if (
        not isinstance(archive_name, str)
        or not archive_name
        or Path(archive_name).name != archive_name
    ):
        raise AuthorityError("bottle audit candidate archive name is malformed")
    archive_size = candidate["archive_size_bytes"]
    if isinstance(archive_size, bool) or not isinstance(archive_size, int) or archive_size <= 0:
        raise AuthorityError("bottle audit candidate archive size is malformed")
    archive_sha256 = candidate["archive_sha256"]
    if (
        not isinstance(archive_sha256, str)
        or len(archive_sha256) != 64
        or any(character not in "0123456789abcdef" for character in archive_sha256)
    ):
        raise AuthorityError("bottle audit candidate archive digest is malformed")
    if "authority_scope" in value:
        raise AuthorityError("bottle audit already declares an authority scope")
    del value["candidate"]
    value["authority_scope"] = {
        "candidate_binding": "none",
        "purpose": "macos-x64 release compatibility inspection",
    }
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def _finalize_member(snapshot_fd: int, member: Member) -> None:
    file_fd, parent_fd, leaf = _open_regular_at(
        snapshot_fd,
        member.snapshot,
        flags=os.O_RDWR,
    )
    try:
        if member.replacements or member.candidate_neutral_audit:
            os.lseek(file_fd, 0, os.SEEK_SET)
            data = b""
            while chunk := os.read(file_fd, 1024 * 1024):
                data += chunk
            if member.candidate_neutral_audit:
                data = _candidate_neutral_bottle_audit(data)
            for old, new in member.replacements:
                if len(new) > len(old) or data.count(old) != 1:
                    raise AuthorityError(f"approved relocation contract changed: {member.snapshot}")
                data = data.replace(old, new + b"\0" * (len(old) - len(new)), 1)
            if b"@@HOMEBREW_PREFIX@@" in data:
                raise AuthorityError(
                    f"approved relocation left a Homebrew placeholder: {member.snapshot}"
                )
            os.lseek(file_fd, 0, os.SEEK_SET)
            os.ftruncate(file_fd, 0)
            view = memoryview(data)
            while view:
                written = os.write(file_fd, view)
                view = view[written:]
            os.fsync(file_fd)
        if _stable_fd_digest(file_fd, parent_fd, leaf) != member.snapshot_sha256:
            raise AuthorityError(f"relocated snapshot digest is not approved: {member.snapshot}")
    finally:
        os.close(file_fd)
        os.close(parent_fd)


def _manifest(policy: Policy) -> bytes:
    value = {
        "schema_version": 1,
        "authority": policy.authority,
        "bottle_sha256": policy.bottle_sha256,
        "llvm_version": policy.version,
        "members": [
            {
                "path": member.snapshot,
                "sha256": member.snapshot_sha256,
                "source_path": member.source,
                "source_sha256": member.source_sha256,
                "transform": (
                    "candidate-neutral-bottle-audit-v1"
                    if member.candidate_neutral_audit
                    else "fixed-width-loader-relocation-v1"
                    if member.replacements
                    else "identity"
                ),
            }
            for member in policy.members
        ],
    }
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def _expected_entries(policy: Policy) -> tuple[set[str], set[str]]:
    files = {member.snapshot for member in policy.members} | {"authority.json"}
    directories: set[str] = set()
    for value in files:
        parent = PurePosixPath(value).parent
        while str(parent) != ".":
            directories.add(str(parent))
            parent = parent.parent
    return files, directories


def _inventory_snapshot(
    directory_fd: int,
    *,
    prefix: PurePosixPath | None = None,
) -> tuple[set[str], set[str]]:
    files: set[str] = set()
    directories: set[str] = set()
    with os.scandir(directory_fd) as entries:
        for entry in entries:
            relative = PurePosixPath(entry.name) if prefix is None else prefix / entry.name
            relative_text = str(relative)
            mode = entry.stat(follow_symlinks=False)
            if stat.S_ISLNK(mode.st_mode):
                raise AuthorityError(f"snapshot contains a symlink: {relative_text}")
            if stat.S_ISDIR(mode.st_mode):
                child_fd = os.open(
                    entry.name,
                    os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                    dir_fd=directory_fd,
                )
                try:
                    opened = os.fstat(child_fd)
                    if _identity(mode) != _identity(opened):
                        raise AuthorityError(
                            f"snapshot directory changed while opening: {relative_text}"
                        )
                    if opened.st_uid != os.geteuid() or stat.S_IMODE(opened.st_mode) != 0o500:
                        raise AuthorityError(
                            f"snapshot directory is mutable or not owned: {relative_text}"
                        )
                    directories.add(relative_text)
                    child_files, child_directories = _inventory_snapshot(
                        child_fd,
                        prefix=relative,
                    )
                    files.update(child_files)
                    directories.update(child_directories)
                finally:
                    os.close(child_fd)
            elif stat.S_ISREG(mode.st_mode):
                files.add(relative_text)
            else:
                raise AuthorityError(f"snapshot contains a non-regular file: {relative_text}")
    return files, directories


def _verify_snapshot_fd(root_fd: int, policy: Policy) -> None:
    root_stat = os.fstat(root_fd)
    if root_stat.st_uid != os.geteuid() or stat.S_IMODE(root_stat.st_mode) != 0o500:
        raise AuthorityError("snapshot root is not owner-private and immutable")
    expected_files, expected_directories = _expected_entries(policy)
    observed_files, observed_directories = _inventory_snapshot(root_fd)
    if observed_files != expected_files or observed_directories != expected_directories:
        raise AuthorityError("snapshot tree differs from the approved closed inventory")
    for member in policy.members:
        file_fd, parent_fd, leaf = _open_regular_at(root_fd, member.snapshot)
        try:
            mode = os.fstat(file_fd)
            if mode.st_uid != os.geteuid() or stat.S_IMODE(mode.st_mode) != member.mode:
                raise AuthorityError(f"snapshot member mode or owner changed: {member.snapshot}")
            if _stable_fd_digest(file_fd, parent_fd, leaf) != member.snapshot_sha256:
                raise AuthorityError(f"snapshot member digest changed: {member.snapshot}")
        finally:
            os.close(file_fd)
            os.close(parent_fd)
    manifest_fd, manifest_parent_fd, manifest_leaf = _open_regular_at(root_fd, "authority.json")
    try:
        manifest_stat = os.fstat(manifest_fd)
        if manifest_stat.st_uid != os.geteuid() or stat.S_IMODE(manifest_stat.st_mode) != 0o400:
            raise AuthorityError("snapshot authority manifest mode or owner changed")
        expected_manifest = _manifest(policy)
        os.lseek(manifest_fd, 0, os.SEEK_SET)
        value = b""
        while chunk := os.read(manifest_fd, 1024 * 1024):
            value += chunk
        if value != expected_manifest:
            raise AuthorityError("snapshot authority manifest changed")
        if _stable_fd_digest(manifest_fd, manifest_parent_fd, manifest_leaf) != hashlib.sha256(
            expected_manifest
        ).hexdigest():
            raise AuthorityError("snapshot authority manifest changed while verifying")
    finally:
        os.close(manifest_fd)
        os.close(manifest_parent_fd)


def _open_verified_snapshot(snapshot_root: str | Path, policy: Policy) -> tuple[Path, int]:
    snapshot = _strict_absolute(str(snapshot_root), name="snapshot root")
    _reject_symlink_ancestors(snapshot, include_leaf=True)
    root_fd = _open_absolute_directory(snapshot)
    try:
        _verify_snapshot_fd(root_fd, policy)
    except Exception:
        os.close(root_fd)
        raise
    return snapshot, root_fd


def verify_snapshot(snapshot_root: str | Path, policy: Policy = APPROVED_POLICY) -> None:
    _, root_fd = _open_verified_snapshot(snapshot_root, policy)
    os.close(root_fd)


def run_verified_tool(
    snapshot_root: str | Path,
    tool: str,
    arguments: Iterable[str],
    policy: Policy = APPROVED_POLICY,
    *,
    before_exec: Callable[[Path], None] | None = None,
) -> int:
    member_path = {
        "readobj": "bin/llvm-readobj",
        "objdump": "bin/llvm-objdump",
    }.get(tool)
    if member_path is None:
        raise AuthorityError(f"unsupported approved LLVM tool: {tool}")
    member = next(
        (candidate for candidate in policy.members if candidate.snapshot == member_path),
        None,
    )
    if member is None or member.mode != 0o500:
        raise AuthorityError(f"approved LLVM tool is absent from policy: {tool}")
    snapshot, root_fd = _open_verified_snapshot(snapshot_root, policy)
    tool_fd = -1
    tool_parent_fd = -1
    try:
        tool_fd, tool_parent_fd, tool_leaf = _open_regular_at(root_fd, member.snapshot)
        tool_stat = os.fstat(tool_fd)
        if tool_stat.st_uid != os.geteuid() or stat.S_IMODE(tool_stat.st_mode) != member.mode:
            raise AuthorityError(f"approved LLVM tool mode or owner changed: {tool}")
        if _stable_fd_digest(tool_fd, tool_parent_fd, tool_leaf) != member.snapshot_sha256:
            raise AuthorityError(f"approved LLVM tool digest changed: {tool}")
        _verify_snapshot_fd(root_fd, policy)
        if _stable_fd_digest(tool_fd, tool_parent_fd, tool_leaf) != member.snapshot_sha256:
            raise AuthorityError(f"approved LLVM tool changed before execution: {tool}")
        if before_exec is not None:
            before_exec(snapshot)
        # Execute the retained, rehashed tool descriptor itself. The child cwd
        # remains anchored to the retained snapshot root, and the only dynamic
        # library search path is its closed, relocated lib directory. Renaming
        # or replacing the caller-visible snapshot cannot redirect either one.
        child = os.fork()
        if child == 0:
            try:
                os.set_inheritable(tool_fd, True)
                os.fchdir(root_fd)
                environment = {
                    name: value
                    for name, value in os.environ.items()
                    if not name.startswith("DYLD_")
                }
                environment["DYLD_LIBRARY_PATH"] = "lib"
                os.execve(
                    f"/dev/fd/{tool_fd}",
                    [PurePosixPath(member.snapshot).name, *arguments],
                    environment,
                )
            except BaseException as error:
                message = f"error: macOS LLVM authority: descriptor-bound exec failed: {error}\n"
                os.write(2, message.encode(errors="replace"))
                os._exit(127)
        while True:
            try:
                _, status = os.waitpid(child, 0)
                break
            except InterruptedError:
                continue
        if os.WIFEXITED(status):
            return os.WEXITSTATUS(status)
        if os.WIFSIGNALED(status):
            return 128 + os.WTERMSIG(status)
        raise AuthorityError("approved LLVM tool entered an unexpected process state")
    finally:
        if tool_fd >= 0:
            os.close(tool_fd)
        if tool_parent_fd >= 0:
            os.close(tool_parent_fd)
        os.close(root_fd)


def create_snapshot(
    task_root: str | Path,
    snapshot_root: str | Path,
    policy: Policy = APPROVED_POLICY,
    *,
    before_final_verify: Callable[[Path], None] | None = None,
) -> None:
    task = _strict_absolute(str(task_root), name="task root")
    snapshot = _strict_absolute(str(snapshot_root), name="snapshot root")
    _reject_symlink_ancestors(task, include_leaf=True)
    _reject_symlink_ancestors(snapshot, include_leaf=False)
    if snapshot == Path(snapshot.anchor):
        raise AuthorityError("snapshot root must not be a filesystem root")
    task_fd = -1
    parent_fd = -1
    snapshot_fd = -1
    try:
        task_fd = _open_absolute_directory(task)
        parent_fd = _open_absolute_directory(snapshot.parent)
        parent_stat = os.fstat(parent_fd)
        if (
            parent_stat.st_uid != os.geteuid()
            or not stat.S_ISDIR(parent_stat.st_mode)
            or stat.S_IMODE(parent_stat.st_mode) & 0o077
        ):
            raise AuthorityError("snapshot parent is not owner-private")
        try:
            os.mkdir(snapshot.name, 0o700, dir_fd=parent_fd)
        except FileExistsError as error:
            raise AuthorityError("snapshot root already exists") from error
        snapshot_fd = os.open(
            snapshot.name,
            os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
            dir_fd=parent_fd,
        )
        os.fchmod(snapshot_fd, 0o700)
        for member in policy.members:
            _copy_member(task_fd, snapshot_fd, member)
        for member in policy.members:
            _finalize_member(snapshot_fd, member)
        manifest_fd = os.open(
            "authority.json",
            os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
            0o600,
            dir_fd=snapshot_fd,
        )
        try:
            view = memoryview(_manifest(policy))
            while view:
                written = os.write(manifest_fd, view)
                view = view[written:]
            os.fsync(manifest_fd)
            os.fchmod(manifest_fd, 0o400)
        finally:
            os.close(manifest_fd)
        for member in policy.members:
            file_fd, member_parent_fd, _ = _open_regular_at(snapshot_fd, member.snapshot)
            try:
                os.fchmod(file_fd, member.mode)
            finally:
                os.close(file_fd)
                os.close(member_parent_fd)
        _, expected_directories = _expected_entries(policy)
        directories = sorted(
            expected_directories,
            key=lambda value: len(PurePosixPath(value).parts),
            reverse=True,
        )
        for directory in directories:
            directory_fd = _open_directory_at(snapshot_fd, directory)
            try:
                os.fchmod(directory_fd, 0o500)
            finally:
                os.close(directory_fd)
        os.fchmod(snapshot_fd, 0o500)
        if before_final_verify is not None:
            before_final_verify(snapshot)
        verify_snapshot(snapshot, policy)
    except Exception:
        if snapshot_fd >= 0:
            os.fchmod(snapshot_fd, 0o700)
            _, expected_directories = _expected_entries(policy)
            for directory in sorted(
                expected_directories,
                key=lambda value: len(PurePosixPath(value).parts),
            ):
                try:
                    directory_fd = _open_directory_at(snapshot_fd, directory)
                except AuthorityError:
                    continue
                try:
                    os.fchmod(directory_fd, 0o700)
                finally:
                    os.close(directory_fd)
            os.close(snapshot_fd)
            snapshot_fd = -1
            shutil.rmtree(snapshot)
        raise
    finally:
        if snapshot_fd >= 0:
            os.close(snapshot_fd)
        if task_fd >= 0:
            os.close(task_fd)
        if parent_fd >= 0:
            os.close(parent_fd)


def authority_summary(policy: Policy = APPROVED_POLICY) -> dict[str, object]:
    return {
        "authority": policy.authority,
        "bottle_sha256": policy.bottle_sha256,
        "llvm_version": policy.version,
        "llvm_readobj_sha256": next(
            member.source_sha256
            for member in policy.members
            if member.snapshot == "bin/llvm-readobj"
        ),
        "llvm_objdump_sha256": next(
            member.source_sha256
            for member in policy.members
            if member.snapshot == "bin/llvm-objdump"
        ),
    }


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    snapshot = subparsers.add_parser("snapshot")
    snapshot.add_argument("--task-root", required=True)
    snapshot.add_argument("--snapshot-root", required=True)
    verify = subparsers.add_parser("verify-snapshot")
    verify.add_argument("--snapshot-root", required=True)
    run = subparsers.add_parser("run-verified")
    run.add_argument("--snapshot-root", required=True)
    run.add_argument("--tool", choices=("readobj", "objdump"), required=True)
    run.add_argument("tool_arguments", nargs=argparse.REMAINDER)
    subparsers.add_parser("authority")
    return parser


def main(argv: Iterable[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        if args.command == "snapshot":
            create_snapshot(args.task_root, args.snapshot_root)
        elif args.command == "verify-snapshot":
            verify_snapshot(args.snapshot_root)
        elif args.command == "run-verified":
            tool_arguments = args.tool_arguments
            if tool_arguments[:1] == ["--"]:
                tool_arguments = tool_arguments[1:]
            return run_verified_tool(args.snapshot_root, args.tool, tool_arguments)
        else:
            print(json.dumps(authority_summary(), sort_keys=True, separators=(",", ":")))
    except (AuthorityError, OSError) as error:
        print(f"error: macOS LLVM authority: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
