#!/usr/bin/env python3
"""Focused transaction tests for the public fixed-pair installer helper."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import signal
import stat
import subprocess
import sys
import tempfile
import time
import unittest


ROOT = Path(__file__).resolve().parents[2]
SLOT_PATHS = {
    "core": Path("bin/ctx"),
    "companion": Path("libexec/ctx-pro"),
    "envelope": Path("share/ctx/managed-pair-envelope.json"),
    "state": Path("share/ctx/managed-pair-state.json"),
}


def load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


installer = load("managed_pair_installer", ROOT / "scripts/install-managed-pair.py")
fixtures = load(
    "managed_pair_contract_fixtures",
    ROOT / "scripts/tests/check-managed-pair-contracts-test.py",
)


class ManagedPairInstallerTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.install_root = self.root / "installation"
        candidate = self.make_candidate("current", 17, "v1.2.3")
        self.core = candidate["core"]
        self.companion = candidate["companion"]
        self.envelope = candidate["envelope"]

    def make_candidate(
        self, name: str, generation: int, release_name: str
    ) -> dict[str, Path]:
        candidate_root = self.root / "candidates" / name
        candidate_root.mkdir(parents=True)
        core = candidate_root / "ctx-linux-x64"
        companion = candidate_root / "ctx-pro-linux-x64"
        envelope = candidate_root / "ctx-managed-pair-linux-x64.json"
        core.write_bytes(
            b"#!/bin/sh\nroot=$(CDPATH= cd -- \"$(dirname \"$0\")/..\" && pwd)\n"
            b"exec \"$root/libexec/ctx-pro\" \"$@\"\n# " + name.encode() + b"\n"
        )
        companion.write_bytes(
            b"#!/bin/sh\nprintf 'fixed companion selected\\n'\n# " + name.encode() + b"\n"
        )
        os.chmod(core, 0o755)
        os.chmod(companion, 0o755)
        self.write_envelope_for(
            core, companion, envelope, generation, release_name
        )
        return {"core": core, "companion": companion, "envelope": envelope}

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def write_envelope(self, generation: int, release_name: str) -> None:
        self.write_envelope_for(
            self.core, self.companion, self.envelope, generation, release_name
        )

    def write_envelope_for(
        self,
        core: Path,
        companion: Path,
        envelope: Path,
        generation: int,
        release_name: str,
    ) -> None:
        manifest = fixtures.manifest("linux-x64")
        manifest["release_authority_key_id"] = "test-staging"
        manifest["rollback_generation"] = generation
        manifest["release_name"] = release_name
        for kind, path in (("core", core), ("companion", companion)):
            value = path.read_bytes()
            component = manifest["components"][kind]
            component["sha256"] = hashlib.sha256(value).hexdigest()
            component["size_bytes"] = len(value)
            component["object_key"] = (
                f"sha256/{component['sha256']}/{component['artifact_name']}"
            )
        signed = fixtures.envelope(manifest)
        envelope.write_bytes(installer.contracts.canonical_payload_bytes(signed))

    def install(self, fault=lambda _: None):
        return self.install_candidate(
            {"core": self.core, "companion": self.companion, "envelope": self.envelope},
            self.install_root,
            fault,
        )

    def install_candidate(
        self,
        candidate: dict[str, Path],
        install_root: Path,
        fault=lambda _: None,
    ):
        return installer.install_pair(
            envelope_path=candidate["envelope"],
            core_path=candidate["core"],
            companion_path=candidate["companion"],
            install_root=install_root,
            target="linux-x64",
            authorities=fixtures.test_authorities(),
            fault=fault,
        )

    def worker_command(
        self,
        candidate: dict[str, Path],
        install_root: Path,
        *extra: str,
    ) -> list[str]:
        return [
            sys.executable,
            str(Path(__file__).resolve()),
            "--worker",
            "--core",
            str(candidate["core"]),
            "--companion",
            str(candidate["companion"]),
            "--envelope",
            str(candidate["envelope"]),
            "--install-root",
            str(install_root),
            *extra,
        ]

    def recovery_worker_command(self, install_root: Path) -> list[str]:
        return [
            sys.executable,
            str(Path(__file__).resolve()),
            "--worker",
            "--recover-only",
            "--install-root",
            str(install_root),
        ]

    def installed_slots(self, install_root: Path) -> dict[str, bytes]:
        return {
            slot: (install_root / relative).read_bytes()
            for slot, relative in SLOT_PATHS.items()
        }

    def candidate_slots(self, candidate: dict[str, Path]) -> dict[str, bytes]:
        envelope = candidate["envelope"].read_bytes()
        manifest, payload = installer.verified_manifest(
            envelope, "linux-x64", fixtures.test_authorities()
        )
        state = installer.state_document(manifest, payload, envelope)
        return {
            "core": candidate["core"].read_bytes(),
            "companion": candidate["companion"].read_bytes(),
            "envelope": envelope,
            "state": installer.state_bytes(state),
        }

    def assert_no_abandoned_transaction(self, install_root: Path) -> None:
        abandoned = [
            path
            for path in install_root.rglob(".*")
            if "managed-pair" in path.name and path.name != installer.LOCK_NAME
        ]
        self.assertEqual(abandoned, [])

    def test_signed_pair_uses_fixed_slots_and_publishes_bridge_state(self) -> None:
        state = self.install()
        installed_core = self.install_root / "bin/ctx"
        installed_companion = self.install_root / "libexec/ctx-pro"
        installed_envelope = self.install_root / "share/ctx/managed-pair-envelope.json"
        installed_state = self.install_root / "share/ctx/managed-pair-state.json"
        self.assertEqual(installed_core.read_bytes(), self.core.read_bytes())
        self.assertEqual(installed_companion.read_bytes(), self.companion.read_bytes())
        self.assertEqual(installed_envelope.read_bytes(), self.envelope.read_bytes())
        self.assertEqual(json.loads(installed_state.read_bytes()), state)
        self.assertTrue(stat.S_IMODE(installed_core.stat().st_mode) & 0o100)
        self.assertTrue(stat.S_IMODE(installed_companion.stat().st_mode) & 0o100)

        completed = subprocess.run(
            [installed_core, "opaque-selection-probe"],
            check=True,
            capture_output=True,
            text=True,
        )
        self.assertEqual(completed.stdout, "fixed companion selected\n")

    @unittest.skipIf(os.name == "nt", "POSIX directory mode contract")
    def test_install_secures_existing_managed_directories_for_bridge_launch(self) -> None:
        for relative in (".", "bin", "libexec", "share", "share/ctx"):
            directory = self.install_root / relative
            directory.mkdir(parents=True, exist_ok=True)
            os.chmod(directory, 0o777)

        self.install()

        for relative in (".", "bin", "libexec", "share", "share/ctx"):
            mode = stat.S_IMODE((self.install_root / relative).stat().st_mode)
            self.assertEqual(mode & 0o022, 0, f"unsafe directory mode for {relative}: {mode:o}")

    def test_failed_replacement_rolls_back_all_four_active_slots(self) -> None:
        self.install()
        before = {
            relative: (self.install_root / relative).read_bytes()
            for relative in (
                "bin/ctx",
                "libexec/ctx-pro",
                "share/ctx/managed-pair-envelope.json",
                "share/ctx/managed-pair-state.json",
            )
        }
        self.core.write_bytes(self.core.read_bytes() + b"# next\n")
        self.companion.write_bytes(self.companion.read_bytes() + b"# next\n")
        self.write_envelope(18, "v1.2.4")

        def fail_after_companion(step: str) -> None:
            if step == "after_activate_companion":
                raise RuntimeError("injected publication fault")

        with self.assertRaisesRegex(RuntimeError, "injected publication fault"):
            self.install(fail_after_companion)
        self.assertEqual(
            before,
            {relative: (self.install_root / relative).read_bytes() for relative in before},
        )

    def test_tampered_signed_metadata_and_downgrade_are_rejected(self) -> None:
        self.install()
        value = json.loads(self.envelope.read_bytes())
        value["signature_base64"] = "A" * len(value["signature_base64"])
        self.envelope.write_bytes(installer.contracts.canonical_payload_bytes(value))
        with self.assertRaisesRegex(installer.InstallError, "signature"):
            self.install()

        self.write_envelope(16, "v1.2.2")
        with self.assertRaisesRegex(installer.InstallError, "downgrade"):
            self.install()

    @unittest.skipUnless(hasattr(signal, "SIGKILL"), "requires SIGKILL crash injection")
    def test_kill_at_every_durable_phase_recovers_on_restart(self) -> None:
        old = {
            "core": self.core,
            "companion": self.companion,
            "envelope": self.envelope,
        }
        candidate = self.make_candidate("restart", 18, "v1.2.4")
        expected_new = self.candidate_slots(candidate)
        last_old_checkpoint = installer.CRASH_CHECKPOINTS.index(
            "before_activate_state"
        )
        first_new_checkpoint = installer.CRASH_CHECKPOINTS.index(
            "after_activate_state"
        )
        self.assertEqual(first_new_checkpoint, last_old_checkpoint + 1)
        for checkpoint in installer.CRASH_CHECKPOINTS:
            with self.subTest(checkpoint=checkpoint):
                install_root = self.root / "crash" / checkpoint
                self.install_candidate(old, install_root)
                expected_old = self.installed_slots(install_root)
                checkpoint_marker = self.root / "crash-markers" / checkpoint
                checkpoint_marker.parent.mkdir(exist_ok=True)
                completed = subprocess.run(
                    self.worker_command(
                        candidate,
                        install_root,
                        "--crash",
                        checkpoint,
                        "--crash-marker",
                        str(checkpoint_marker),
                    ),
                    capture_output=True,
                    text=True,
                    check=False,
                )
                self.assertEqual(
                    completed.returncode,
                    -signal.SIGKILL,
                    completed.stdout + completed.stderr,
                )
                self.assertEqual(
                    checkpoint_marker.read_text(encoding="utf-8"),
                    f"{checkpoint}\n",
                )
                recovered = subprocess.run(
                    self.recovery_worker_command(install_root),
                    capture_output=True,
                    text=True,
                    check=False,
                )
                self.assertEqual(
                    recovered.returncode,
                    0,
                    recovered.stdout + recovered.stderr,
                )
                checkpoint_index = installer.CRASH_CHECKPOINTS.index(checkpoint)
                if checkpoint_index <= last_old_checkpoint:
                    expected_slots = expected_old
                    expected_recovery = "rolled_back"
                else:
                    self.assertGreaterEqual(checkpoint_index, first_new_checkpoint)
                    expected_slots = expected_new
                    expected_recovery = "committed"
                self.assertEqual(recovered.stdout, f"{expected_recovery}\n")
                self.assertEqual(self.installed_slots(install_root), expected_slots)
                self.assert_no_abandoned_transaction(install_root)

    def test_concurrent_installers_serialize_and_leave_the_newest_pair(self) -> None:
        first = self.make_candidate("concurrent-18", 18, "v1.2.4")
        second = self.make_candidate("concurrent-19", 19, "v1.2.5")
        self.install()
        marker = self.root / "first-holds-lock"
        first_process = subprocess.Popen(
            self.worker_command(
                first,
                self.install_root,
                "--pause",
                "before_activate_core",
                "--pause-marker",
                str(marker),
                "--pause-seconds",
                "0.8",
            ),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        second_process = None
        try:
            deadline = time.monotonic() + 5
            while not marker.exists() and time.monotonic() < deadline:
                if first_process.poll() is not None:
                    break
                time.sleep(0.02)
            self.assertTrue(marker.exists(), "first installer did not reach its locked pause")
            second_process = subprocess.Popen(
                self.worker_command(second, self.install_root),
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            time.sleep(0.15)
            self.assertIsNone(second_process.poll(), "second installer bypassed the install lock")
            first_stdout, first_stderr = first_process.communicate(timeout=10)
            second_stdout, second_stderr = second_process.communicate(timeout=10)
            self.assertEqual(first_process.returncode, 0, first_stdout + first_stderr)
            self.assertEqual(second_process.returncode, 0, second_stdout + second_stderr)
        finally:
            for process in (first_process, second_process):
                if process is not None and process.poll() is None:
                    process.kill()
                    process.wait(timeout=5)

        state = json.loads(
            (self.install_root / "share/ctx/managed-pair-state.json").read_text()
        )
        self.assertEqual(state["identity"]["release_name"], "v1.2.5")
        self.assertEqual(
            (self.install_root / "bin/ctx").read_bytes(), second["core"].read_bytes()
        )
        self.assertEqual(
            (self.install_root / "libexec/ctx-pro").read_bytes(),
            second["companion"].read_bytes(),
        )
        self.assert_no_abandoned_transaction(self.install_root)

    def test_install_root_and_fixed_directories_reject_symlinks(self) -> None:
        attacker = self.root / "attacker"
        attacker.mkdir()
        unsafe_root = self.root / "unsafe"
        unsafe_root.symlink_to(attacker, target_is_directory=True)
        with self.assertRaisesRegex(installer.InstallError, "real directory"):
            self.install_candidate(
                {"core": self.core, "companion": self.companion, "envelope": self.envelope},
                unsafe_root,
            )

        unsafe_fixed = self.root / "unsafe-fixed"
        unsafe_fixed.mkdir()
        (unsafe_fixed / "bin").symlink_to(attacker, target_is_directory=True)
        with self.assertRaisesRegex(installer.InstallError, "real directory"):
            self.install_candidate(
                {"core": self.core, "companion": self.companion, "envelope": self.envelope},
                unsafe_fixed,
            )

        unsafe_lock = self.root / "unsafe-lock"
        (unsafe_lock / "bin").mkdir(parents=True)
        (unsafe_lock / "libexec").mkdir()
        (unsafe_lock / "share/ctx").mkdir(parents=True)
        (unsafe_lock / installer.LOCK_NAME).symlink_to(self.root / "attacker-lock")
        with self.assertRaisesRegex(installer.InstallError, "symlink"):
            self.install_candidate(
                {"core": self.core, "companion": self.companion, "envelope": self.envelope},
                unsafe_lock,
            )


def worker_main(arguments: list[str]) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--core", type=Path)
    parser.add_argument("--companion", type=Path)
    parser.add_argument("--envelope", type=Path)
    parser.add_argument("--install-root", required=True, type=Path)
    parser.add_argument("--recover-only", action="store_true")
    parser.add_argument("--crash", default="")
    parser.add_argument("--crash-marker", type=Path)
    parser.add_argument("--pause", default="")
    parser.add_argument("--pause-marker", type=Path)
    parser.add_argument("--pause-seconds", type=float, default=0.0)
    args = parser.parse_args(arguments)

    if args.recover_only:
        recovered = installer.recover_pair(
            install_root=args.install_root, target="linux-x64"
        )
        print(recovered or "clean")
        return 0
    if args.core is None or args.companion is None or args.envelope is None:
        parser.error("--core, --companion, and --envelope are required for installation")

    def fault(checkpoint: str) -> None:
        if checkpoint == args.pause:
            if args.pause_marker is None:
                raise RuntimeError("pause marker is required")
            with args.pause_marker.open("wb") as marker:
                marker.write(b"locked\n")
                marker.flush()
                os.fsync(marker.fileno())
            time.sleep(args.pause_seconds)
        if checkpoint == args.crash:
            if args.crash_marker is None:
                raise RuntimeError("crash marker is required")
            with args.crash_marker.open("w", encoding="utf-8") as marker:
                marker.write(f"{checkpoint}\n")
                marker.flush()
                os.fsync(marker.fileno())
            os.kill(os.getpid(), signal.SIGKILL)

    installer.install_pair(
        envelope_path=args.envelope,
        core_path=args.core,
        companion_path=args.companion,
        install_root=args.install_root,
        target="linux-x64",
        authorities=fixtures.test_authorities(),
        fault=fault,
    )
    return 0


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--worker":
        raise SystemExit(worker_main(sys.argv[2:]))
    unittest.main()
