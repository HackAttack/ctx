#!/usr/bin/env python3
from __future__ import annotations

import ast
import datetime
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/release/run-with-release-advisory-inputs.py"
UPDATE_SCRIPT = ROOT / "scripts/update-release-advisory-db.py"
SPEC = importlib.util.spec_from_file_location("release_advisory_inputs", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class Response(io.BytesIO):
    def __enter__(self):
        return self

    def __exit__(self, *_args):
        self.close()


class ReleaseAdvisoryInputsTest(unittest.TestCase):
    def test_database_updater_uses_python_3_9_datetime_api(self) -> None:
        source = UPDATE_SCRIPT.read_text(encoding="utf-8")
        tree = ast.parse(
            source,
            filename=str(UPDATE_SCRIPT),
            feature_version=(3, 9),
        )
        datetime_imports = {
            alias.name
            for node in ast.walk(tree)
            if isinstance(node, ast.ImportFrom) and node.module == "datetime"
            for alias in node.names
        }
        self.assertNotIn("UTC", datetime_imports)
        self.assertIn("timezone", datetime_imports)

        namespace = {"__name__": "release_advisory_database_update_test"}
        exec(compile(tree, str(UPDATE_SCRIPT), "exec"), namespace)
        self.assertIs(namespace["UTC"], datetime.timezone.utc)

    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        (self.root / "security").mkdir()
        (self.root / "scripts").mkdir()
        (self.root / "scripts/update-release-advisory-db.py").write_text(
            "raise SystemExit('test must intercept updater')\n",
            encoding="utf-8",
        )
        self.scanner_bytes = b"fixture scanner\n"
        self.scanner_sha256 = hashlib.sha256(self.scanner_bytes).hexdigest()
        (self.root / "security/release-advisory-policy-v1.json").write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "scanner": {
                        "name": "osv-scanner",
                        "version": "2.4.0",
                        "sha256_by_target": {
                            target: self.scanner_sha256
                            for target in MODULE.SCANNER_ASSETS
                        },
                    },
                }
            ),
            encoding="utf-8",
        )

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def fake_run(self, argv, **kwargs):
        self.assertNotIn("BUILDKITE_API_ACCESS_TOKEN", kwargs["env"])
        database = Path(argv[argv.index("--database-root") + 1])
        metadata = Path(argv[argv.index("--metadata") + 1])
        database.mkdir(parents=True)
        metadata.write_text("{}\n", encoding="utf-8")
        return subprocess.CompletedProcess(argv, 0, "", "")

    def fake_prepared_inputs(self, *_args):
        scanner = self.root / "scanner"
        database = self.root / "database"
        metadata = self.root / "metadata.json"
        scanner.write_bytes(self.scanner_bytes)
        database.mkdir(exist_ok=True)
        metadata.write_text("{}\n", encoding="utf-8")
        return scanner, database, metadata, self.scanner_sha256

    def test_every_policy_target_has_an_exact_upstream_asset(self) -> None:
        self.assertEqual(
            MODULE.SCANNER_ASSETS,
            {
                "linux-x64": "osv-scanner_linux_amd64",
                "linux-arm64": "osv-scanner_linux_arm64",
                "macos-arm64": "osv-scanner_darwin_arm64",
                "macos-x64": "osv-scanner_darwin_amd64",
                "windows-x64": "osv-scanner_windows_amd64.exe",
            },
        )

    def test_loads_single_platform_policy_for_its_exact_target(self) -> None:
        policy = self.root / "single-platform-policy.json"
        policy.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "scanner": {
                        "name": "osv-scanner",
                        "version": "2.4.0",
                        "platform": "linux-x64",
                        "sha256": self.scanner_sha256,
                    },
                }
            ),
            encoding="utf-8",
        )
        self.assertEqual(
            MODULE.load_scanner_spec(policy, "linux-x64"),
            ("2.4.0", self.scanner_sha256, "osv-scanner_linux_amd64"),
        )

    def test_single_platform_policy_rejects_other_target(self) -> None:
        policy = self.root / "single-platform-policy.json"
        policy.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "scanner": {
                        "name": "osv-scanner",
                        "version": "2.4.0",
                        "platform": "linux-x64",
                        "sha256": self.scanner_sha256,
                    },
                }
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(MODULE.InputError, "does not cover target"):
            MODULE.load_scanner_spec(policy, "macos-x64")

    def test_prepares_checked_scanner_and_offline_database(self) -> None:
        task_root = self.root / "task"
        task_root.mkdir()
        with mock.patch.object(
            MODULE.urllib.request,
            "urlopen",
            return_value=Response(self.scanner_bytes),
        ) as urlopen, mock.patch.object(
            MODULE.subprocess,
            "run",
            side_effect=self.fake_run,
        ):
            scanner, database, metadata, digest = MODULE.prepare_inputs(
                self.root,
                task_root,
                "linux-arm64",
            )
        self.assertEqual(scanner.read_bytes(), self.scanner_bytes)
        self.assertTrue(os.access(scanner, os.X_OK))
        self.assertTrue(database.is_dir())
        self.assertTrue(metadata.is_file())
        self.assertEqual(digest, self.scanner_sha256)
        self.assertEqual(
            urlopen.call_args.args[0].full_url,
            "https://github.com/google/osv-scanner/releases/download/"
            "v2.4.0/osv-scanner_linux_arm64",
        )

    def test_rejects_scanner_before_database_update_on_digest_mismatch(self) -> None:
        task_root = self.root / "bad-task"
        task_root.mkdir()
        with mock.patch.object(
            MODULE.urllib.request,
            "urlopen",
            return_value=Response(b"tampered scanner\n"),
        ), mock.patch.object(MODULE.subprocess, "run") as run:
            with self.assertRaisesRegex(MODULE.InputError, "digest does not match"):
                MODULE.prepare_inputs(self.root, task_root, "macos-x64")
        run.assert_not_called()
        self.assertFalse((task_root / "scanner/osv-scanner").exists())

    def test_wrapped_release_command_retains_full_environment(self) -> None:
        release_environment = {
            "APPLE_SIGNING_IDENTITY": "sentinel-signing-secret",
            "BUILDKITE_AGENT_ACCESS_TOKEN": "sentinel-buildkite-secret",
            "NOTARYTOOL_PASSWORD": "sentinel-notary-secret",
        }
        observed_environment = {}

        def fake_release_run(argv, **kwargs):
            self.assertEqual(argv, ["release-command"])
            observed_environment.update(kwargs["env"])
            return subprocess.CompletedProcess(argv, 0)

        with mock.patch.object(
            MODULE,
            "ROOT",
            self.root,
        ), mock.patch.object(
            MODULE,
            "prepare_inputs",
            side_effect=self.fake_prepared_inputs,
        ), mock.patch.object(
            MODULE.subprocess,
            "run",
            side_effect=fake_release_run,
        ), mock.patch.object(
            MODULE.os,
            "environ",
            release_environment,
        ), mock.patch.object(
            MODULE.sys,
            "argv",
            [str(SCRIPT), "--target", "linux-x64", "--", "release-command"],
        ):
            self.assertEqual(MODULE.main(), 0)

        for name, value in release_environment.items():
            self.assertEqual(observed_environment[name], value)
        self.assertEqual(observed_environment["CTX_OSV_SCANNER"], str(self.root / "scanner"))
        self.assertEqual(
            observed_environment["CTX_OSV_DATABASE_DIR"],
            str(self.root / "database"),
        )
        self.assertEqual(
            observed_environment["CTX_OSV_DATABASE_METADATA"],
            str(self.root / "metadata.json"),
        )


if __name__ == "__main__":
    unittest.main()
