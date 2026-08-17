#!/usr/bin/env python3

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/dependency-advisory-gate.py"
GATE = Path(sys.argv.pop(1)).resolve(strict=True)
FIXTURES = ROOT / "scripts/tests/fixtures/dependency-advisory"
FAKE_SCANNER = FIXTURES / "fake-osv-scanner.py"
NOW = "2026-07-29T17:00:00Z"


class AdvisoryGateTest(unittest.TestCase):
    def test_release_runtime_uses_python_3_10_datetime_api(self) -> None:
        import tomli

        source = SCRIPT.read_text(encoding="utf-8")
        self.assertNotIn("from datetime import UTC", source)
        self.assertIn("UTC = timezone.utc", source)
        self.assertIn("import tomli as tomllib", source)
        self.assertNotIn("import tomllib", source)
        self.assertEqual(tomli.__version__, "2.0.1")

    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        (self.repo / "Cargo.lock").write_text("fixture lock\n", encoding="utf-8")
        self.scanner = self.root / "fake-osv-scanner.py"
        shutil.copy2(FAKE_SCANNER, self.scanner)
        self.scanner.chmod(0o700)
        self.scanner_config = self.scanner.with_suffix(".config.json")
        self.scanner_environment_receipt = self.root / "scanner-environments.jsonl"
        self.policy = self.repo / "policy.json"
        self.policy.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "scanner": {
                        "authority": "ctx-release-osv-linux-x64-v1",
                        "name": "osv-scanner",
                        "platform": "linux-x64",
                        "version": "2.4.0",
                        "sha256": hashlib.sha256(self.scanner.read_bytes()).hexdigest(),
                    },
                    "lockfiles": [
                        {
                            "path": "Cargo.lock",
                            "ecosystem": "crates.io",
                            "disposition": "scan",
                            "closure": "lockfile",
                            "role": "fixture release closure",
                        }
                    ],
                }
            ),
            encoding="utf-8",
        )
        self.exceptions = self.repo / "exceptions.json"
        self.write_exceptions([])
        self.database_root = self.root / "database"
        database = self.database_root / "osv-scanner/crates.io/all.zip"
        database.parent.mkdir(parents=True)
        database.write_bytes(b"fixture advisory database\n")
        self.metadata = self.root / "metadata.json"
        self.metadata.write_text(
            json.dumps(
                {
                    "schema_version": 2,
                    "sealed_at": NOW,
                    "databases": [
                        {
                            "ecosystem": "crates.io",
                            "path": "osv-scanner/crates.io/all.zip",
                            "sha256": hashlib.sha256(database.read_bytes()).hexdigest(),
                            "size": database.stat().st_size,
                            "source_generation": "123456789",
                            "source_last_modified": "2026-07-29T16:00:00Z",
                            "source_url": (
                                "https://osv-vulnerabilities.storage.googleapis.com/"
                                "crates.io/all.zip"
                            ),
                        }
                    ],
                }
            ),
            encoding="utf-8",
        )
        self.receipt = self.root / "receipt.json"

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def write_exceptions(self, entries: list[dict[str, str]]) -> None:
        self.exceptions.write_text(
            json.dumps({"schema_version": 1, "exceptions": entries}),
            encoding="utf-8",
        )

    @staticmethod
    def exception(expires: str) -> dict[str, str]:
        return {
            "advisory_id": "RUSTSEC-2099-0001",
            "ecosystem": "crates.io",
            "package": "unsafe-crate",
            "version": "1.2.3",
            "lockfile": "Cargo.lock",
            "rationale": "Reviewed fixture risk is accepted for this bounded test.",
            "owner": "fixture-release-owner",
            "expires": expires,
        }

    def run_gate(
        self, fixture: str, scanner_exit: int = 0
    ) -> tuple[subprocess.CompletedProcess[str], dict[str, object]]:
        self.scanner_environment_receipt.unlink(missing_ok=True)
        self.scanner_config.write_text(
            json.dumps(
                {
                    "environment_receipt": str(self.scanner_environment_receipt),
                    "exit_code": scanner_exit,
                    "fixture": str(FIXTURES / fixture),
                }
            ),
            encoding="utf-8",
        )
        environment = os.environ.copy()
        environment.update(
            {
                "APPLE_SIGNING_IDENTITY": "sentinel-signing-secret",
                "BUILDKITE_AGENT_ACCESS_TOKEN": "sentinel-buildkite-secret",
                "NOTARYTOOL_PASSWORD": "sentinel-notary-secret",
                "OSV_SCANNER_CONFIG": "sentinel-ambient-config",
                "OSV_SCANNER_LOCAL_DB_CACHE_DIRECTORY": "sentinel-ambient-database",
            }
        )
        result = subprocess.run(
            [
                str(GATE),
                "--repo-root",
                str(self.repo),
                "--policy",
                str(self.policy),
                "--exceptions",
                str(self.exceptions),
                "--database-root",
                str(self.database_root),
                "--database-metadata",
                str(self.metadata),
                "--scanner",
                str(self.scanner),
                "--target-id",
                "fixture",
                "--output",
                str(self.receipt),
                "--now",
                NOW,
            ],
            text=True,
            capture_output=True,
            env=environment,
        )
        return result, json.loads(self.receipt.read_text(encoding="utf-8"))

    def scanner_environments(self) -> list[dict[str, object]]:
        return [
            json.loads(line)
            for line in self.scanner_environment_receipt.read_text(
                encoding="utf-8"
            ).splitlines()
        ]

    def test_clean(self) -> None:
        result, receipt = self.run_gate("osv-clean.json")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(receipt["status"], "clean")
        self.assertFalse(receipt["coverage"]["os_packages_scanned"])

    def test_scanner_subprocesses_receive_only_allowlisted_environment(self) -> None:
        result, _receipt = self.run_gate("osv-clean.json")
        self.assertEqual(result.returncode, 0, result.stderr)
        invocations = self.scanner_environments()
        self.assertEqual(len(invocations), 2)
        version, scan = invocations
        self.assertEqual(version["arguments"], ["--version"])
        self.assertNotIn("OSV_SCANNER_LOCAL_DB_CACHE_DIRECTORY", version["environment"])
        self.assertIn("scan", scan["arguments"])
        self.assertEqual(
            scan["environment"]["OSV_SCANNER_LOCAL_DB_CACHE_DIRECTORY"],
            str(self.database_root),
        )
        for invocation in invocations:
            environment = invocation["environment"]
            self.assertNotIn("APPLE_SIGNING_IDENTITY", environment)
            self.assertNotIn("BUILDKITE_AGENT_ACCESS_TOKEN", environment)
            self.assertNotIn("NOTARYTOOL_PASSWORD", environment)
            self.assertNotIn("OSV_SCANNER_CONFIG", environment)
            self.assertLessEqual(
                set(environment),
                {
                    *{
                        "LC_CTYPE",
                        "SystemRoot",
                        "SYSTEMROOT",
                        "TEMP",
                        "TMP",
                        "TMPDIR",
                        "WINDIR",
                    },
                    "OSV_SCANNER_LOCAL_DB_CACHE_DIRECTORY",
                },
            )

    def test_unreviewed_advisory(self) -> None:
        result, receipt = self.run_gate("osv-advisory.json", 1)
        self.assertEqual(result.returncode, 10)
        self.assertEqual(receipt["status"], "advisory")
        self.assertEqual(receipt["summary"]["unreviewed_advisory_count"], 1)

    def test_reviewed_exception(self) -> None:
        self.write_exceptions([self.exception("2026-07-30")])
        result, receipt = self.run_gate("osv-advisory.json", 1)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(receipt["status"], "clean")
        self.assertEqual(receipt["summary"]["reviewed_exception_count"], 1)

    def test_expired_exception(self) -> None:
        self.write_exceptions([self.exception("2026-07-28")])
        result, receipt = self.run_gate("osv-advisory.json", 1)
        self.assertEqual(result.returncode, 11)
        self.assertEqual(receipt["status"], "expired_exception")

    def test_unknown_exception(self) -> None:
        self.write_exceptions([self.exception("2026-07-30")])
        result, receipt = self.run_gate("osv-clean.json")
        self.assertEqual(result.returncode, 12)
        self.assertEqual(receipt["status"], "unknown_exception")

    def test_tool_failure(self) -> None:
        result, receipt = self.run_gate("osv-clean.json", 7)
        self.assertEqual(result.returncode, 21)
        self.assertEqual(receipt["status"], "tool_failure")

    def test_scanner_digest_mismatch(self) -> None:
        policy = json.loads(self.policy.read_text(encoding="utf-8"))
        policy["scanner"]["sha256"] = "0" * 64
        self.policy.write_text(json.dumps(policy), encoding="utf-8")
        result, receipt = self.run_gate("osv-clean.json")
        self.assertEqual(result.returncode, 21)
        self.assertEqual(receipt["status"], "tool_failure")
        self.assertEqual(receipt["failure_reason"], "OSV-Scanner digest mismatch")

    def test_scanner_target_must_be_pinned(self) -> None:
        policy = json.loads(self.policy.read_text(encoding="utf-8"))
        policy["scanner"]["platform"] = "linux-arm64"
        self.policy.write_text(json.dumps(policy), encoding="utf-8")
        result, receipt = self.run_gate("osv-clean.json")
        self.assertEqual(result.returncode, 21)
        self.assertEqual(receipt["status"], "tool_failure")
        self.assertEqual(
            receipt["failure_reason"],
            "advisory scanner policy is invalid",
        )

    def test_latest_official_generation_is_accepted_regardless_of_age(self) -> None:
        metadata = json.loads(self.metadata.read_text(encoding="utf-8"))
        metadata["databases"][0]["source_last_modified"] = "2020-01-01T00:00:00Z"
        self.metadata.write_text(json.dumps(metadata), encoding="utf-8")
        result, receipt = self.run_gate("osv-clean.json")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(receipt["status"], "clean")

    def test_unofficial_database_source_is_rejected(self) -> None:
        metadata = json.loads(self.metadata.read_text(encoding="utf-8"))
        metadata["databases"][0]["source_url"] = "https://example.com/all.zip"
        self.metadata.write_text(json.dumps(metadata), encoding="utf-8")
        result, receipt = self.run_gate("osv-clean.json")
        self.assertEqual(result.returncode, 21)
        self.assertEqual(receipt["status"], "tool_failure")
        self.assertEqual(
            receipt["failure_reason"],
            "OSV database source is invalid: crates.io",
        )

    def test_legacy_unsealed_database_metadata_is_rejected(self) -> None:
        metadata = json.loads(self.metadata.read_text(encoding="utf-8"))
        metadata["schema_version"] = 1
        metadata.pop("sealed_at")
        self.metadata.write_text(json.dumps(metadata), encoding="utf-8")
        result, receipt = self.run_gate("osv-clean.json")
        self.assertEqual(result.returncode, 21)
        self.assertEqual(receipt["status"], "tool_failure")
        self.assertEqual(
            receipt["failure_reason"],
            "OSV database metadata schema is unsupported",
        )

    def test_unknown_lockfile(self) -> None:
        (self.repo / "package-lock.json").write_text("{}\n", encoding="utf-8")
        result, receipt = self.run_gate("osv-clean.json")
        self.assertEqual(result.returncode, 21)
        self.assertEqual(receipt["status"], "tool_failure")
        self.assertIn("unreviewed dependency lockfiles", receipt["failure_reason"])


if __name__ == "__main__":
    unittest.main()
