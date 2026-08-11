#!/usr/bin/env python3
from __future__ import annotations

import errno
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import stat
import sys
import tempfile
import unittest
from unittest import mock


MODULE_PATH = Path(__file__).parents[1] / "release" / "macos_llvm_authority.py"
SPEC = importlib.util.spec_from_file_location("macos_llvm_authority", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
AUTHORITY = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = AUTHORITY
SPEC.loader.exec_module(AUTHORITY)


def digest(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


APPROVED_READER = (
    b"#!/bin/sh\n"
    b'if [ -n "${APPROVED_TOOL_MARKER:-}" ]; then\n'
    b'  : > "${APPROVED_TOOL_MARKER}"\n'
    b"fi\n"
    b'printf "Homebrew LLVM version 22.1.8\\n"\n'
)
APPROVED_OBJDUMP = b'#!/bin/sh\nprintf "Homebrew LLVM version 22.1.8\\n"\n'
LIB_SOURCE = b"prefix @@HOMEBREW_PREFIX@@/opt/example/lib.dylib suffix"
LIB_SNAPSHOT = b"prefix @loader_path/lib.dylib" + b"\0" * (
    len(b"@@HOMEBREW_PREFIX@@/opt/example/lib.dylib") - len(b"@loader_path/lib.dylib")
) + b" suffix"
STALE_AUDIT_VALUE = {
    "schema_version": 1,
    "candidate": {
        "target": "macos-x64",
        "public_commit": "1" * 40,
        "private_commit": "2" * 40,
        "archive_name": "stale-pair.tar.gz",
        "archive_size_bytes": 123,
        "archive_sha256": "3" * 64,
    },
    "tool_authority": {"llvm_version": "22.1.8"},
}
STALE_AUDIT = (json.dumps(STALE_AUDIT_VALUE, indent=2) + "\n").encode()
NEUTRAL_AUDIT_VALUE = {
    "schema_version": 1,
    "authority_scope": {
        "candidate_binding": "none",
        "purpose": "macos-x64 release compatibility inspection",
    },
    "tool_authority": {"llvm_version": "22.1.8"},
}
NEUTRAL_AUDIT = (
    json.dumps(NEUTRAL_AUDIT_VALUE, sort_keys=True, separators=(",", ":")) + "\n"
).encode()
FIXTURE_POLICY = AUTHORITY.Policy(
    authority="fixture approved authority",
    bottle_sha256="a" * 64,
    version="22.1.8",
    members=(
        AUTHORITY.Member(
            "payload/bin/llvm-readobj",
            "bin/llvm-readobj",
            digest(APPROVED_READER),
            digest(APPROVED_READER),
            0o500,
        ),
        AUTHORITY.Member(
            "payload/bin/llvm-objdump",
            "bin/llvm-objdump",
            digest(APPROVED_OBJDUMP),
            digest(APPROVED_OBJDUMP),
            0o500,
        ),
        AUTHORITY.Member(
            "payload/lib/lib.dylib",
            "lib/lib.dylib",
            digest(LIB_SOURCE),
            digest(LIB_SNAPSHOT),
            0o400,
            ((b"@@HOMEBREW_PREFIX@@/opt/example/lib.dylib", b"@loader_path/lib.dylib"),),
        ),
        AUTHORITY.Member(
            "BOTTLE-AUDIT.json",
            "provenance/BOTTLE-AUDIT.json",
            digest(STALE_AUDIT),
            digest(NEUTRAL_AUDIT),
            0o400,
            candidate_neutral_audit=True,
        ),
    ),
)


class MacosLlvmAuthorityTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="ctx-macos-llvm-authority-test.")
        self.root = Path(self.temporary.name)
        os.chmod(self.root, 0o700)
        self.task = self.root / "task"
        (self.task / "payload/bin").mkdir(parents=True)
        (self.task / "payload/lib").mkdir(parents=True)
        (self.task / "payload/bin/llvm-readobj").write_bytes(APPROVED_READER)
        (self.task / "payload/bin/llvm-objdump").write_bytes(APPROVED_OBJDUMP)
        (self.task / "payload/lib/lib.dylib").write_bytes(LIB_SOURCE)
        (self.task / "BOTTLE-AUDIT.json").write_bytes(STALE_AUDIT)
        for tool in ("llvm-readobj", "llvm-objdump"):
            os.chmod(self.task / "payload/bin" / tool, 0o500)
        self.snapshot = self.root / "snapshot"

    def tearDown(self) -> None:
        for current, directory_names, file_names in os.walk(self.root, topdown=False):
            os.chmod(current, 0o700)
            for name in file_names:
                path = Path(current) / name
                if not path.is_symlink():
                    os.chmod(path, 0o600)
            for name in directory_names:
                path = Path(current) / name
                if not path.is_symlink():
                    os.chmod(path, 0o700)
        self.temporary.cleanup()

    def create(self, **kwargs: object) -> None:
        AUTHORITY.create_snapshot(self.task, self.snapshot, FIXTURE_POLICY, **kwargs)

    def test_approved_snapshot_is_closed_private_and_verifiable(self) -> None:
        self.create()
        AUTHORITY.verify_snapshot(self.snapshot, FIXTURE_POLICY)
        self.assertEqual(stat.S_IMODE(self.snapshot.stat().st_mode), 0o500)
        self.assertEqual((self.snapshot / "lib/lib.dylib").read_bytes(), LIB_SNAPSHOT)
        audit_path = self.snapshot / "provenance/BOTTLE-AUDIT.json"
        self.assertEqual(audit_path.read_bytes(), NEUTRAL_AUDIT)
        self.assertNotIn(b'"candidate"', audit_path.read_bytes())
        self.assertNotIn(b"stale-pair", audit_path.read_bytes())

    def test_malformed_candidate_specific_audit_is_rejected(self) -> None:
        malformed = dict(STALE_AUDIT_VALUE)
        malformed["candidate"] = {"target": "macos-x64"}
        with self.assertRaisesRegex(AUTHORITY.AuthorityError, "candidate binding is malformed"):
            AUTHORITY._candidate_neutral_bottle_audit(json.dumps(malformed).encode())

    def test_tool_symlink_is_rejected(self) -> None:
        reader = self.task / "payload/bin/llvm-readobj"
        reader.unlink()
        reader.symlink_to("llvm-objdump")
        with self.assertRaisesRegex(AUTHORITY.AuthorityError, "without symlinks"):
            self.create()

    def test_ancestor_symlink_is_rejected(self) -> None:
        alias = self.root / "task-alias"
        alias.symlink_to(self.task, target_is_directory=True)
        with self.assertRaisesRegex(AUTHORITY.AuthorityError, "symlink ancestor"):
            AUTHORITY.create_snapshot(alias, self.snapshot, FIXTURE_POLICY)

    def test_created_stage_physical_path_accepts_var_style_alias(self) -> None:
        physical_parent = self.root / "private-var-folders"
        physical_parent.mkdir()
        alias = self.root / "var-folders"
        alias.symlink_to(physical_parent, target_is_directory=True)
        created_stage = alias / "ctx-bazel-release.created"
        created_stage.mkdir()
        os.chmod(created_stage, 0o700)
        aliased_snapshot = created_stage / "snapshot"
        with self.assertRaisesRegex(AUTHORITY.AuthorityError, "symlink ancestor"):
            AUTHORITY.create_snapshot(self.task, aliased_snapshot, FIXTURE_POLICY)
        physical_stage = created_stage.resolve(strict=True)
        AUTHORITY.create_snapshot(self.task, physical_stage / "snapshot", FIXTURE_POLICY)
        AUTHORITY.verify_snapshot(physical_stage / "snapshot", FIXTURE_POLICY)

    def test_traversal_is_rejected_before_resolution(self) -> None:
        traversal = f"{self.task}/payload/../.."
        with self.assertRaisesRegex(AUTHORITY.AuthorityError, "traversal"):
            AUTHORITY.create_snapshot(traversal, self.snapshot, FIXTURE_POLICY)

    def test_replacement_race_is_rejected_by_final_rehash(self) -> None:
        def replace(snapshot: Path) -> None:
            os.chmod(snapshot, 0o700)
            os.chmod(snapshot / "bin", 0o700)
            reader = snapshot / "bin/llvm-readobj"
            os.chmod(reader, 0o600)
            reader.write_bytes(b"replaced after verified copy\n")
            os.chmod(reader, 0o500)
            os.chmod(snapshot / "bin", 0o500)
            os.chmod(snapshot, 0o500)

        with self.assertRaisesRegex(AUTHORITY.AuthorityError, "digest changed"):
            self.create(before_final_verify=replace)

    def test_path_substitution_at_execution_boundary_uses_retained_snapshot(self) -> None:
        self.create()
        retained = self.root / "retained-snapshot"
        approved_marker = self.root / "approved-ran"
        forged_marker = self.root / "forged-ran"

        def substitute(snapshot: Path) -> None:
            os.chmod(snapshot, 0o700)
            snapshot.rename(retained)
            os.chmod(retained, 0o500)
            (snapshot / "bin").mkdir(parents=True)
            forged = snapshot / "bin/llvm-readobj"
            forged.write_text(
                f"#!/bin/sh\n: > {forged_marker}\nexit 99\n",
                encoding="utf-8",
            )
            os.chmod(forged, 0o500)

        with mock.patch.dict(os.environ, {"APPROVED_TOOL_MARKER": str(approved_marker)}):
            status = AUTHORITY.run_verified_tool(
                self.snapshot,
                "readobj",
                ("--version",),
                FIXTURE_POLICY,
                before_exec=substitute,
            )
        self.assertEqual(status, 0)
        self.assertTrue(approved_marker.exists())
        self.assertFalse(forged_marker.exists())

    def test_execution_does_not_depend_on_executable_dev_fd(self) -> None:
        self.create()
        approved_marker = self.root / "approved-ran"
        observed_path = self.root / "observed-exec-path"
        native_execve = os.execve

        def reject_descriptor_exec(
            path: str,
            argv: list[str],
            environment: dict[str, str],
        ) -> None:
            path_text = os.fspath(path)
            observed_path.write_text(path_text, encoding="utf-8")
            if path_text.startswith("/dev/fd/"):
                raise PermissionError(errno.EACCES, os.strerror(errno.EACCES), path_text)
            native_execve(path, argv, environment)

        with (
            mock.patch.dict(os.environ, {"APPROVED_TOOL_MARKER": str(approved_marker)}),
            mock.patch.object(AUTHORITY.os, "execve", side_effect=reject_descriptor_exec),
        ):
            status = AUTHORITY.run_verified_tool(
                self.snapshot,
                "readobj",
                ("--version",),
                FIXTURE_POLICY,
            )
        self.assertEqual(status, 0)
        self.assertEqual(observed_path.read_text(encoding="utf-8"), "bin/llvm-readobj")
        self.assertTrue(approved_marker.exists())

    def test_mutation_at_execution_boundary_is_rejected(self) -> None:
        self.create()
        forged_marker = self.root / "forged-ran"

        def mutate(snapshot: Path) -> None:
            os.chmod(snapshot, 0o700)
            os.chmod(snapshot / "bin", 0o700)
            reader = snapshot / "bin/llvm-readobj"
            os.chmod(reader, 0o700)
            reader.write_text(
                f"#!/bin/sh\n: > {forged_marker}\nexit 99\n",
                encoding="utf-8",
            )
            os.chmod(reader, 0o500)
            os.chmod(snapshot / "bin", 0o500)
            os.chmod(snapshot, 0o500)

        with self.assertRaisesRegex(AUTHORITY.AuthorityError, "digest changed"):
            AUTHORITY.run_verified_tool(
                self.snapshot,
                "readobj",
                ("--version",),
                FIXTURE_POLICY,
                before_exec=mutate,
            )
        self.assertFalse(forged_marker.exists())

    def test_matching_unapproved_version_is_rejected_without_execution(self) -> None:
        marker = self.root / "spoof-ran"
        spoof = (
            b"#!/bin/sh\n"
            + f"touch {marker}\n".encode()
            + b'printf "Homebrew LLVM version 22.1.8\\n"\n'
        )
        reader = self.task / "payload/bin/llvm-readobj"
        os.chmod(reader, 0o600)
        reader.write_bytes(spoof)
        os.chmod(reader, 0o500)
        with self.assertRaisesRegex(AUTHORITY.AuthorityError, "digest is not approved"):
            self.create()
        self.assertFalse(marker.exists())

    def test_spoofed_executable_is_rejected(self) -> None:
        reader = self.task / "payload/bin/llvm-readobj"
        os.chmod(reader, 0o600)
        reader.write_bytes(b"not an approved executable\n")
        os.chmod(reader, 0o500)
        with self.assertRaisesRegex(AUTHORITY.AuthorityError, "digest is not approved"):
            self.create()


if __name__ == "__main__":
    unittest.main()
