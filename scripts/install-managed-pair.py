#!/usr/bin/env python3
"""Verify and durably publish one signed fixed ctx companion pair."""

from __future__ import annotations

import argparse
from contextlib import contextmanager
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import stat
import sys
import time
import uuid
from typing import Any, Callable, Iterator, Mapping


ROOT = Path(__file__).resolve().parents[1]
CONTRACT_CHECKER = ROOT / "scripts" / "check-managed-pair-contracts.py"
SPEC = importlib.util.spec_from_file_location("ctx_managed_pair_contracts", CONTRACT_CHECKER)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("cannot load managed-pair contract checker")
contracts = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(contracts)

_SCRIPT_DIRECTORY = os.fspath(ROOT / "scripts")
sys.path.insert(0, _SCRIPT_DIRECTORY)
try:
    from managed_pair_installer.transaction_contract import *
finally:
    del sys.path[0]
del _SCRIPT_DIRECTORY


class InstallError(ValueError):
    """A signed pair cannot be safely installed or recovered."""


def digest(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def _open_nofollow(path: Path, flags: int, mode: int = 0o600) -> int:
    nofollow = getattr(os, "O_NOFOLLOW", 0)
    cloexec = getattr(os, "O_CLOEXEC", 0)
    before = None
    try:
        before = path.lstat()
    except FileNotFoundError:
        pass
    if before is not None and stat.S_ISLNK(before.st_mode):
        raise InstallError(f"refusing symlink path: {path}")
    descriptor = os.open(path, flags | nofollow | cloexec, mode)
    opened = os.fstat(descriptor)
    try:
        named = path.lstat()
    except OSError:
        os.close(descriptor)
        raise
    if (
        stat.S_ISLNK(named.st_mode)
        or (named.st_dev, named.st_ino) != (opened.st_dev, opened.st_ino)
        or (
            before is not None
            and (before.st_dev, before.st_ino) != (opened.st_dev, opened.st_ino)
        )
    ):
        os.close(descriptor)
        raise InstallError(f"path identity changed while opening: {path}")
    return descriptor


def read_regular(path: Path, label: str, maximum: int) -> bytes:
    try:
        descriptor = _open_nofollow(path, os.O_RDONLY)
    except OSError as error:
        raise InstallError(f"{label} is unavailable: {path}") from error
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
            raise InstallError(f"{label} is not an identity-safe regular file: {path}")
        if metadata.st_size <= 0 or metadata.st_size > maximum:
            raise InstallError(f"{label} is outside its size bound: {path}")
        chunks: list[bytes] = []
        remaining = metadata.st_size
        while remaining:
            chunk = os.read(descriptor, min(1024 * 1024, remaining))
            if not chunk:
                break
            chunks.append(chunk)
            remaining -= len(chunk)
        value = b"".join(chunks)
        after = os.fstat(descriptor)
        named = path.lstat()
        if (
            len(value) != metadata.st_size
            or (metadata.st_dev, metadata.st_ino, metadata.st_size)
            != (after.st_dev, after.st_ino, after.st_size)
            or stat.S_ISLNK(named.st_mode)
            or (named.st_dev, named.st_ino) != (after.st_dev, after.st_ino)
        ):
            raise InstallError(f"{label} changed while it was read: {path}")
        return value
    finally:
        os.close(descriptor)


def parse_envelope(bytes_value: bytes) -> dict[str, Any]:
    try:
        value = json.loads(
            bytes_value.decode("utf-8"),
            object_pairs_hook=contracts.unique_object,
            parse_constant=lambda _: (_ for _ in ()).throw(ValueError()),
        )
    except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
        raise InstallError("signed managed-pair envelope is malformed") from error
    if not isinstance(value, dict):
        raise InstallError("signed managed-pair envelope must contain an object")
    return value


def verified_manifest(
    envelope_bytes: bytes,
    target: str,
    authorities: Mapping[str, Mapping[str, str]] | None = None,
) -> tuple[dict[str, Any], bytes]:
    envelope = parse_envelope(envelope_bytes)
    try:
        manifest = contracts.validate_envelope(envelope, authorities=authorities)
    except contracts.ContractError as error:
        raise InstallError(f"signed managed-pair envelope is invalid: {error}") from error
    if manifest.get("contract") != "ctx-managed-pair-manifest":
        raise InstallError("signed managed-pair metadata is not a target manifest")
    if manifest["target"]["id"] != target:
        raise InstallError(
            f"signed managed-pair target is {manifest['target']['id']}, expected {target}"
        )
    payload_bytes = contracts.canonical_payload_bytes(manifest)
    return manifest, payload_bytes


def component_bytes(path: Path, component: Mapping[str, Any], label: str) -> bytes:
    value = read_regular(path, label, MAX_COMPONENT_BYTES)
    if len(value) != component["size_bytes"] or digest(value) != component["sha256"]:
        raise InstallError(f"{label} does not match its signed identity")
    return value


def state_document(
    manifest: Mapping[str, Any], payload_bytes: bytes, envelope_bytes: bytes
) -> dict[str, Any]:
    components = manifest["components"]
    return {
        "contract": "ctx-managed-pair-state",
        "schema_version": 1,
        "identity": {
            "release_name": manifest["release_name"],
            "target": manifest["target"]["id"],
            "rollback_generation": manifest["rollback_generation"],
            "manifest_sha256": digest(payload_bytes),
            "core": {
                "sha256": components["core"]["sha256"],
                "size_bytes": components["core"]["size_bytes"],
            },
            "companion": {
                "sha256": components["companion"]["sha256"],
                "size_bytes": components["companion"]["size_bytes"],
            },
        },
        "envelope_sha256": digest(envelope_bytes),
        "envelope_size_bytes": len(envelope_bytes),
    }


def state_bytes(value: Mapping[str, Any]) -> bytes:
    encoded = (json.dumps(value, ensure_ascii=False, indent=2) + "\n").encode("utf-8")
    if len(encoded) > MAX_STATE_BYTES:
        raise InstallError("managed-pair state exceeds its size bound")
    return encoded


def fsync_directory(path: Path) -> None:
    if os.name == "nt":
        # Every Windows rename below uses MOVEFILE_WRITE_THROUGH. Python cannot
        # portably open a directory for fsync on Windows.
        return
    flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise InstallError(f"cannot open managed-pair directory for fsync: {path}") from error
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISDIR(metadata.st_mode):
            raise InstallError(f"managed-pair path is not a directory: {path}")
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def ensure_directory_tree(path: Path) -> None:
    if not path.is_absolute():
        raise InstallError("managed-pair install root must be absolute")
    anchor = Path(path.anchor)
    current = anchor
    for part in path.parts[1:]:
        if part in {"", ".", ".."}:
            raise InstallError("managed-pair install root must be a normalized absolute path")
        candidate = current / part
        try:
            metadata = candidate.lstat()
        except FileNotFoundError:
            try:
                candidate.mkdir()
            except OSError as error:
                raise InstallError(f"cannot create managed-pair directory: {candidate}") from error
            fsync_directory(current)
            metadata = candidate.lstat()
        except OSError as error:
            raise InstallError(f"cannot inspect managed-pair directory: {candidate}") from error
        if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
            raise InstallError(f"managed-pair directory is not a real directory: {candidate}")
        current = candidate


def ensure_child_directory(parent: Path, name: str) -> Path:
    child = parent / name
    try:
        metadata = child.lstat()
    except FileNotFoundError:
        try:
            child.mkdir()
        except OSError as error:
            raise InstallError(f"cannot create managed-pair directory: {child}") from error
        fsync_directory(parent)
        metadata = child.lstat()
    except OSError as error:
        raise InstallError(f"cannot inspect managed-pair directory: {child}") from error
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
        raise InstallError(f"managed-pair directory is not a real directory: {child}")
    return child


class ManagedLayout:
    def __init__(
        self,
        root: Path,
        target: str,
        paths: Mapping[str, Path],
        control: Path,
        directory_identities: Mapping[Path, tuple[int, int]],
    ) -> None:
        self.root = root
        self.target = target
        self.paths = paths
        self.control = control
        self.directory_identities = directory_identities

    def assert_safe(self) -> None:
        for directory, identity in self.directory_identities.items():
            try:
                metadata = directory.lstat()
            except OSError as error:
                raise InstallError(
                    f"managed-pair directory identity is unavailable: {directory}"
                ) from error
            if (
                stat.S_ISLNK(metadata.st_mode)
                or not stat.S_ISDIR(metadata.st_mode)
                or (metadata.st_dev, metadata.st_ino) != identity
            ):
                raise InstallError(f"managed-pair directory identity changed: {directory}")

    def transaction_path(self, slot: str, attempt: str, suffix: str) -> Path:
        active = self.paths[slot]
        return active.with_name(f".{active.name}.managed-pair-{attempt}.{suffix}")


def layout(install_root: Path, target: str) -> ManagedLayout:
    ensure_directory_tree(install_root)
    bin_dir = ensure_child_directory(install_root, "bin")
    libexec_dir = ensure_child_directory(install_root, "libexec")
    share_dir = ensure_child_directory(install_root, "share")
    control = ensure_child_directory(share_dir, "ctx")
    suffix = ".exe" if target == "windows-x64" else ""
    paths = {
        "core": bin_dir / f"ctx{suffix}",
        "companion": libexec_dir / f"ctx-pro{suffix}",
        "envelope": control / "managed-pair-envelope.json",
        "state": control / "managed-pair-state.json",
    }
    directories = (install_root, bin_dir, libexec_dir, share_dir, control)
    identities = {
        directory: (directory.lstat().st_dev, directory.lstat().st_ino)
        for directory in directories
    }
    result = ManagedLayout(install_root, target, paths, control, identities)
    result.assert_safe()
    return result


def path_present(path: Path) -> bool:
    try:
        path.lstat()
        return True
    except FileNotFoundError:
        return False
    except OSError as error:
        raise InstallError(f"cannot inspect managed-pair path: {path}") from error


def file_identity(path: Path, label: str, maximum: int) -> dict[str, Any]:
    value = read_regular(path, label, maximum)
    return {"sha256": digest(value), "size_bytes": len(value)}


def optional_identity(path: Path, label: str, maximum: int) -> dict[str, Any] | None:
    if not path_present(path):
        return None
    return file_identity(path, label, maximum)


def identities_equal(left: Mapping[str, Any] | None, right: Mapping[str, Any] | None) -> bool:
    return left == right


def validate_retained(
    paths: Mapping[str, Path],
    candidate_state: Mapping[str, Any],
    authorities: Mapping[str, Mapping[str, str]] | None,
) -> None:
    present = {name: path_present(path) for name, path in paths.items()}
    if not present["state"]:
        if present["companion"] or present["envelope"]:
            raise InstallError("refusing a partial managed pair without its state marker")
        if present["core"]:
            read_regular(paths["core"], "legacy retained Core", MAX_COMPONENT_BYTES)
        return
    if not all(present.values()):
        raise InstallError("retained managed pair is incomplete")
    retained_state_bytes = read_regular(
        paths["state"], "retained managed-pair state", MAX_STATE_BYTES
    )
    try:
        retained_state = json.loads(
            retained_state_bytes.decode("utf-8"),
            object_pairs_hook=contracts.unique_object,
            parse_constant=lambda _: (_ for _ in ()).throw(ValueError()),
        )
        contracts.validate_state(retained_state)
    except (UnicodeDecodeError, json.JSONDecodeError, ValueError, contracts.ContractError) as error:
        raise InstallError("retained managed-pair state is invalid") from error
    retained_envelope = read_regular(
        paths["envelope"], "retained managed-pair envelope", MAX_ENVELOPE_BYTES
    )
    retained_manifest, retained_payload = verified_manifest(
        retained_envelope, retained_state["identity"]["target"], authorities
    )
    expected = state_document(retained_manifest, retained_payload, retained_envelope)
    if retained_state != expected:
        raise InstallError("retained managed-pair state does not match its signed envelope")
    component_bytes(paths["core"], retained_manifest["components"]["core"], "retained Core")
    component_bytes(
        paths["companion"],
        retained_manifest["components"]["companion"],
        "retained companion",
    )
    retained_identity = retained_state["identity"]
    candidate_identity = candidate_state["identity"]
    if candidate_identity["rollback_generation"] < retained_identity["rollback_generation"]:
        raise InstallError("managed-pair rollback generation would downgrade the installation")
    if (
        candidate_identity["rollback_generation"] == retained_identity["rollback_generation"]
        and candidate_identity != retained_identity
    ):
        raise InstallError("managed-pair identity changed without advancing rollback generation")


def write_staged(path: Path, value: bytes, executable: bool) -> None:
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    try:
        descriptor = _open_nofollow(path, flags, 0o700 if executable else 0o600)
    except OSError as error:
        raise InstallError(f"cannot create managed-pair staged file: {path}") from error
    try:
        offset = 0
        while offset < len(value):
            written = os.write(descriptor, value[offset:])
            if written <= 0:
                raise InstallError(f"could not fully write managed-pair staged file: {path}")
            offset += written
        os.fchmod(descriptor, 0o755 if executable else 0o600)
        os.fsync(descriptor)
        named = path.lstat()
        opened = os.fstat(descriptor)
        if stat.S_ISLNK(named.st_mode) or (named.st_dev, named.st_ino) != (
            opened.st_dev,
            opened.st_ino,
        ):
            raise InstallError(f"staged file identity changed while writing: {path}")
    finally:
        os.close(descriptor)
    fsync_directory(path.parent)


def _windows_replace(source: Path, destination: Path) -> None:
    import ctypes

    move = ctypes.windll.kernel32.MoveFileExW
    move.argtypes = (ctypes.c_wchar_p, ctypes.c_wchar_p, ctypes.c_uint32)
    move.restype = ctypes.c_int
    replace_existing = 0x1
    write_through = 0x8
    if not move(str(source), str(destination), replace_existing | write_through):
        raise ctypes.WinError()


def durable_replace(source: Path, destination: Path, managed_layout: ManagedLayout) -> None:
    managed_layout.assert_safe()
    source_metadata = source.lstat()
    if stat.S_ISLNK(source_metadata.st_mode) or not stat.S_ISREG(source_metadata.st_mode):
        raise InstallError(f"managed-pair rename source is not a regular file: {source}")
    if path_present(destination):
        destination_metadata = destination.lstat()
        if stat.S_ISLNK(destination_metadata.st_mode) or not stat.S_ISREG(
            destination_metadata.st_mode
        ):
            raise InstallError(
                f"managed-pair rename destination is not a regular file: {destination}"
            )
    if os.name == "nt":
        _windows_replace(source, destination)
    else:
        os.replace(source, destination)
        fsync_directory(source.parent)
        if source.parent != destination.parent:
            fsync_directory(destination.parent)
    managed_layout.assert_safe()


def durable_unlink(path: Path, managed_layout: ManagedLayout) -> None:
    managed_layout.assert_safe()
    metadata = path.lstat()
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
        raise InstallError(f"managed-pair transaction path is not a regular file: {path}")
    path.unlink()
    fsync_directory(path.parent)
    managed_layout.assert_safe()


def remove_if_present(path: Path, managed_layout: ManagedLayout) -> None:
    if path_present(path):
        durable_unlink(path, managed_layout)


def identity_map(paths: Mapping[str, Path]) -> dict[str, dict[str, Any] | None]:
    return {
        slot: optional_identity(paths[slot], f"retained {slot}", SLOT_MAXIMUMS[slot])
        for slot in SLOTS
    }


def journal_bytes(value: Mapping[str, Any]) -> bytes:
    encoded = (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()
    if len(encoded) > MAX_JOURNAL_BYTES:
        raise InstallError("managed-pair transaction journal exceeds its size bound")
    return encoded


def validate_identity(value: Any, *, optional: bool) -> None:
    if value is None and optional:
        return
    if not isinstance(value, dict) or set(value) != {"sha256", "size_bytes"}:
        raise InstallError("managed-pair journal has an invalid file identity")
    if not isinstance(value["sha256"], str) or not re.fullmatch(
        r"[0-9a-f]{64}", value["sha256"]
    ):
        raise InstallError("managed-pair journal has an invalid SHA-256 identity")
    if (
        not isinstance(value["size_bytes"], int)
        or isinstance(value["size_bytes"], bool)
        or value["size_bytes"] <= 0
        or value["size_bytes"] > MAX_COMPONENT_BYTES
    ):
        raise InstallError("managed-pair journal has an invalid size identity")


def validate_journal(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != {
        "attempt",
        "contract",
        "new",
        "old",
        "phase",
        "schema_version",
        "target",
    }:
        raise InstallError("managed-pair transaction journal has an invalid shape")
    if value["contract"] != JOURNAL_CONTRACT or value["schema_version"] != 1:
        raise InstallError("managed-pair transaction journal has an unknown contract")
    if not isinstance(value["attempt"], str) or not re.fullmatch(
        r"[0-9a-f]{32}", value["attempt"]
    ):
        raise InstallError("managed-pair transaction journal has an invalid attempt")
    if value["target"] not in contracts.TARGET_IDS:
        raise InstallError("managed-pair transaction journal has an invalid target")
    if value["phase"] not in TRANSACTION_PHASES:
        raise InstallError("managed-pair transaction journal has an invalid phase")
    for group, optional in (("old", True), ("new", False)):
        identities = value[group]
        if not isinstance(identities, dict) or set(identities) != set(SLOTS):
            raise InstallError("managed-pair transaction journal has invalid slot identities")
        for slot in SLOTS:
            validate_identity(identities[slot], optional=optional)
            if identities[slot] is not None and identities[slot]["size_bytes"] > SLOT_MAXIMUMS[slot]:
                raise InstallError("managed-pair transaction journal slot exceeds its bound")
    return value


def parse_journal(value: bytes) -> dict[str, Any]:
    try:
        decoded = json.loads(
            value.decode("utf-8"),
            object_pairs_hook=contracts.unique_object,
            parse_constant=lambda _: (_ for _ in ()).throw(ValueError()),
        )
    except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
        raise InstallError("managed-pair transaction journal is malformed") from error
    return validate_journal(decoded)


def journal_path(managed_layout: ManagedLayout) -> Path:
    return managed_layout.control / JOURNAL_NAME


def journal_temp_path(managed_layout: ManagedLayout) -> Path:
    return managed_layout.control / JOURNAL_TEMP_NAME


def recover_legacy_journal_namespace(managed_layout: ManagedLayout) -> None:
    legacy = managed_layout.control / LEGACY_JOURNAL_NAME
    legacy_temporary = managed_layout.control / LEGACY_JOURNAL_TEMP_NAME
    if path_present(legacy_temporary):
        remove_if_present(legacy_temporary, managed_layout)
    if not path_present(legacy):
        return
    if path_present(journal_path(managed_layout)) or path_present(journal_temp_path(managed_layout)):
        raise InstallError("both legacy and bootstrap managed-pair journals are active")
    # The old Python journal contract is recovered by Python under a dedicated
    # namespace. It is intentionally never relabeled as the incompatible Rust
    # transaction-v1 contract.
    durable_replace(legacy, journal_path(managed_layout), managed_layout)


def write_journal(managed_layout: ManagedLayout, journal: Mapping[str, Any]) -> None:
    validate_journal(dict(journal))
    temporary = journal_temp_path(managed_layout)
    remove_if_present(temporary, managed_layout)
    write_staged(temporary, journal_bytes(journal), False)
    durable_replace(temporary, journal_path(managed_layout), managed_layout)


def read_journal(managed_layout: ManagedLayout) -> dict[str, Any] | None:
    path = journal_path(managed_layout)
    if not path_present(path):
        return None
    return parse_journal(read_regular(path, "managed-pair transaction journal", MAX_JOURNAL_BYTES))


@contextmanager
def install_lock(managed_layout: ManagedLayout) -> Iterator[None]:
    # Share the upgrade engine's install-root lock so installer publication and
    # in-product managed-pair activation cannot overlap.
    path = managed_layout.root / LOCK_NAME
    existed = path_present(path)
    try:
        descriptor = _open_nofollow(path, os.O_RDWR | os.O_CREAT, 0o600)
    except OSError as error:
        raise InstallError(f"cannot open managed-pair install lock: {path}") from error
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
            raise InstallError("managed-pair install lock is not identity-safe")
        if metadata.st_size == 0:
            os.write(descriptor, b"\0")
            os.fsync(descriptor)
        elif metadata.st_size != 1:
            raise InstallError("managed-pair install lock has an invalid size")
        if not existed:
            fsync_directory(path.parent)
        if os.name == "nt":
            import msvcrt

            os.lseek(descriptor, 0, os.SEEK_SET)
            while True:
                try:
                    msvcrt.locking(descriptor, msvcrt.LK_NBLCK, 1)
                    break
                except OSError:
                    time.sleep(0.05)
        else:
            import fcntl

            fcntl.flock(descriptor, fcntl.LOCK_EX)
        managed_layout.assert_safe()
        yield
    finally:
        if os.name == "nt":
            import msvcrt

            try:
                os.lseek(descriptor, 0, os.SEEK_SET)
                msvcrt.locking(descriptor, msvcrt.LK_UNLCK, 1)
            except OSError:
                pass
        else:
            import fcntl

            fcntl.flock(descriptor, fcntl.LOCK_UN)
        os.close(descriptor)


def transaction_paths(
    managed_layout: ManagedLayout, journal: Mapping[str, Any], suffix: str
) -> dict[str, Path]:
    return {
        slot: managed_layout.transaction_path(slot, journal["attempt"], suffix)
        for slot in SLOTS
    }


def current_identity(
    path: Path, slot: str, label: str
) -> dict[str, Any] | None:
    return optional_identity(path, label, SLOT_MAXIMUMS[slot])


def require_identity(
    path: Path,
    slot: str,
    expected: Mapping[str, Any],
    label: str,
) -> None:
    actual = current_identity(path, slot, label)
    if not identities_equal(actual, expected):
        raise InstallError(f"{label} does not match the transaction journal")


def active_is_complete_new(
    managed_layout: ManagedLayout, journal: Mapping[str, Any]
) -> bool:
    return all(
        identities_equal(
            current_identity(managed_layout.paths[slot], slot, f"active {slot}"),
            journal["new"][slot],
        )
        for slot in SLOTS
    )


def remove_transaction_file(
    path: Path,
    slot: str,
    managed_layout: ManagedLayout,
    expected: Mapping[str, Any] | None,
    *,
    allow_partial: bool,
) -> None:
    if not path_present(path):
        return
    if not allow_partial:
        require_identity(path, slot, expected, f"transaction {slot}")  # type: ignore[arg-type]
    else:
        metadata = path.lstat()
        if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
            raise InstallError(f"transaction file is not identity-safe: {path}")
        if metadata.st_size > SLOT_MAXIMUMS[slot]:
            raise InstallError(f"transaction file exceeds its bound: {path}")
    durable_unlink(path, managed_layout)


def cleanup_transaction_files(
    managed_layout: ManagedLayout, journal: Mapping[str, Any]
) -> None:
    staged = transaction_paths(managed_layout, journal, "new")
    backups = transaction_paths(managed_layout, journal, "old")
    for slot in SLOTS:
        remove_transaction_file(
            staged[slot], slot, managed_layout, journal["new"][slot], allow_partial=True
        )
    for slot in SLOTS:
        old = journal["old"][slot]
        if path_present(backups[slot]):
            if old is None:
                raise InstallError("unexpected managed-pair backup without an old identity")
            remove_transaction_file(
                backups[slot], slot, managed_layout, old, allow_partial=False
            )
    remove_if_present(journal_temp_path(managed_layout), managed_layout)


def remove_journal(managed_layout: ManagedLayout) -> None:
    remove_if_present(journal_path(managed_layout), managed_layout)


def rollback_transaction(
    managed_layout: ManagedLayout, journal: Mapping[str, Any]
) -> None:
    staged = transaction_paths(managed_layout, journal, "new")
    backups = transaction_paths(managed_layout, journal, "old")

    # A state marker is the activation commit point. Remove an uncommitted new
    # marker before restoring any old component, and restore the old marker last.
    active_state = current_identity(
        managed_layout.paths["state"], "state", "active managed-pair state"
    )
    backup_state = current_identity(backups["state"], "state", "backed-up state")
    old_state = journal["old"]["state"]
    new_state = journal["new"]["state"]
    if backup_state is not None:
        if old_state is None or not identities_equal(backup_state, old_state):
            raise InstallError("backed-up managed-pair state has an unexpected identity")
        if active_state is not None:
            if not identities_equal(active_state, new_state):
                raise InstallError("active managed-pair state has an unexpected identity")
            durable_unlink(managed_layout.paths["state"], managed_layout)
    elif old_state is None:
        if active_state is not None:
            if not identities_equal(active_state, new_state):
                raise InstallError("active managed-pair state has an unexpected identity")
            durable_unlink(managed_layout.paths["state"], managed_layout)
    elif not identities_equal(active_state, old_state):
        raise InstallError("old managed-pair state cannot be recovered")

    for slot in ("core", "companion", "envelope"):
        active = current_identity(managed_layout.paths[slot], slot, f"active {slot}")
        backup = current_identity(backups[slot], slot, f"backed-up {slot}")
        old = journal["old"][slot]
        new = journal["new"][slot]
        if backup is not None:
            if old is None or not identities_equal(backup, old):
                raise InstallError(f"backed-up {slot} has an unexpected identity")
            if active is not None:
                if not identities_equal(active, new):
                    raise InstallError(f"active {slot} has an unexpected identity")
                durable_unlink(managed_layout.paths[slot], managed_layout)
            durable_replace(backups[slot], managed_layout.paths[slot], managed_layout)
        elif old is None:
            if active is not None:
                if not identities_equal(active, new):
                    raise InstallError(f"active {slot} has an unexpected identity")
                durable_unlink(managed_layout.paths[slot], managed_layout)
        elif not identities_equal(active, old):
            raise InstallError(f"old {slot} cannot be recovered")

    if backup_state is not None:
        if path_present(managed_layout.paths["state"]):
            raise InstallError("managed-pair state destination is occupied during rollback")
        durable_replace(backups["state"], managed_layout.paths["state"], managed_layout)

    cleanup_transaction_files(managed_layout, journal)
    remove_journal(managed_layout)


def recover_abandoned(install_root: Path, current_layout: ManagedLayout) -> str | None:
    recover_legacy_journal_namespace(current_layout)
    remove_if_present(journal_temp_path(current_layout), current_layout)
    journal = read_journal(current_layout)
    if journal is None:
        return None
    recovered_layout = (
        current_layout
        if journal["target"] == current_layout.target
        else layout(install_root, journal["target"])
    )
    if active_is_complete_new(recovered_layout, journal):
        cleanup_transaction_files(recovered_layout, journal)
        remove_journal(recovered_layout)
        return "committed"
    rollback_transaction(recovered_layout, journal)
    return "rolled_back"


def recover_pair(*, install_root: Path, target: str) -> str | None:
    managed_layout = layout(install_root, target)
    with install_lock(managed_layout):
        return recover_abandoned(install_root, managed_layout)


def move_old_slot(
    managed_layout: ManagedLayout,
    journal: Mapping[str, Any],
    slot: str,
    backups: Mapping[str, Path],
) -> None:
    old = journal["old"][slot]
    active = current_identity(managed_layout.paths[slot], slot, f"active {slot}")
    if old is None:
        if active is not None:
            raise InstallError(f"unexpected active {slot} appeared during installation")
        return
    if not identities_equal(active, old):
        raise InstallError(f"active {slot} changed during installation")
    if path_present(backups[slot]):
        raise InstallError(f"managed-pair backup already exists for {slot}")
    durable_replace(managed_layout.paths[slot], backups[slot], managed_layout)


def activate_new_slot(
    managed_layout: ManagedLayout,
    journal: Mapping[str, Any],
    slot: str,
    staged: Mapping[str, Path],
) -> None:
    if path_present(managed_layout.paths[slot]):
        raise InstallError(f"managed-pair activation destination is occupied for {slot}")
    require_identity(staged[slot], slot, journal["new"][slot], f"staged {slot}")
    durable_replace(staged[slot], managed_layout.paths[slot], managed_layout)


def install_pair(
    *,
    envelope_path: Path,
    core_path: Path,
    companion_path: Path,
    install_root: Path,
    target: str,
    authorities: Mapping[str, Mapping[str, str]] | None = None,
    fault: Callable[[str], None] = lambda _: None,
) -> dict[str, Any]:
    envelope = read_regular(envelope_path, "signed managed-pair envelope", MAX_ENVELOPE_BYTES)
    manifest, payload = verified_manifest(envelope, target, authorities)
    core = component_bytes(core_path, manifest["components"]["core"], "Core component")
    companion = component_bytes(
        companion_path, manifest["components"]["companion"], "companion component"
    )
    state = state_document(manifest, payload, envelope)
    encoded_state = state_bytes(state)
    values = {
        "core": core,
        "companion": companion,
        "envelope": envelope,
        "state": encoded_state,
    }
    new_identities = {
        slot: {"sha256": digest(value), "size_bytes": len(value)}
        for slot, value in values.items()
    }
    managed_layout = layout(install_root, target)

    with install_lock(managed_layout):
        recover_abandoned(install_root, managed_layout)
        validate_retained(managed_layout.paths, state, authorities)
        old_identities = identity_map(managed_layout.paths)
        journal: dict[str, Any] = {
            "attempt": uuid.uuid4().hex,
            "contract": JOURNAL_CONTRACT,
            "new": new_identities,
            "old": old_identities,
            "phase": "stage_core",
            "schema_version": 1,
            "target": target,
        }
        staged = transaction_paths(managed_layout, journal, "new")
        backups = transaction_paths(managed_layout, journal, "old")

        def transition(phase: str, operation: Callable[[], None]) -> None:
            journal["phase"] = phase
            write_journal(managed_layout, journal)
            fault(f"before_{phase}")
            operation()
            fault(f"after_{phase}")

        try:
            for slot in SLOTS:
                transition(
                    f"stage_{slot}",
                    lambda slot=slot: write_staged(
                        staged[slot], values[slot], slot in {"core", "companion"}
                    ),
                )
            transition(
                "deactivate_state",
                lambda: move_old_slot(managed_layout, journal, "state", backups),
            )
            for slot in ("core", "companion", "envelope"):
                transition(
                    f"backup_{slot}",
                    lambda slot=slot: move_old_slot(
                        managed_layout, journal, slot, backups
                    ),
                )
            for slot in ("core", "companion", "envelope", "state"):
                transition(
                    f"activate_{slot}",
                    lambda slot=slot: activate_new_slot(
                        managed_layout, journal, slot, staged
                    ),
                )
            validate_retained(managed_layout.paths, state, authorities)
            transition("committed", lambda: None)

            journal["phase"] = "cleanup"
            write_journal(managed_layout, journal)
            fault("before_cleanup")
            cleanup_transaction_files(managed_layout, journal)
            fault("after_cleanup_files")
            remove_journal(managed_layout)
        except BaseException:
            recover_abandoned(install_root, managed_layout)
            raise
    return state


def write_json(path: Path, value: Mapping[str, Any]) -> None:
    encoded = (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    write_staged(temporary, encoded, False)
    os.replace(temporary, path)
    fsync_directory(path.parent)


def inspect_envelope(
    envelope_path: Path,
    target: str,
    authorities: Mapping[str, Mapping[str, str]] | None = None,
) -> dict[str, Any]:
    envelope = read_regular(envelope_path, "signed managed-pair envelope", MAX_ENVELOPE_BYTES)
    manifest, _ = verified_manifest(envelope, target, authorities)
    return {
        "release_name": manifest["release_name"],
        "target": target,
        "channel": manifest["channel"],
        "rollback_generation": manifest["rollback_generation"],
        "core": manifest["components"]["core"],
        "companion": manifest["components"]["companion"],
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    inspect = subparsers.add_parser("inspect")
    install = subparsers.add_parser("install")
    for command in (inspect, install):
        command.add_argument("--envelope", required=True, type=Path)
        command.add_argument("--target", required=True, choices=contracts.TARGET_IDS)
    inspect.add_argument("--output", required=True, type=Path)
    install.add_argument("--core", required=True, type=Path)
    install.add_argument("--companion", required=True, type=Path)
    install.add_argument("--install-root", required=True, type=Path)
    args = parser.parse_args()
    try:
        if args.command == "inspect":
            write_json(args.output, inspect_envelope(args.envelope, args.target))
        else:
            state = install_pair(
                envelope_path=args.envelope,
                core_path=args.core,
                companion_path=args.companion,
                install_root=args.install_root,
                target=args.target,
            )
            print(json.dumps(state, sort_keys=True, separators=(",", ":")))
    except (InstallError, OSError, contracts.ContractError) as error:
        print(f"install-managed-pair.py: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
