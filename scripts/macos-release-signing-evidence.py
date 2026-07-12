#!/usr/bin/env python3
"""Create and verify macOS standalone release signing evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
from pathlib import Path
from typing import Any


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def read_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise SystemExit(f"invalid JSON evidence {path}: {error}") from error
    if not isinstance(value, dict):
        raise SystemExit(f"JSON evidence must be an object: {path}")
    return value


def write_json(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    payload = json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n"
    temporary = path.with_name(f".{path.name}.tmp.{os.getpid()}")
    try:
        temporary.write_text(payload, encoding="utf-8")
        os.chmod(temporary, 0o644)
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def read_single_line(path: Path) -> str:
    value = path.read_text(encoding="utf-8").strip()
    if not re.fullmatch(r"[0-9a-fA-F]{64}", value):
        raise SystemExit(f"checksum sidecar is not a SHA-256 digest: {path}")
    return value.lower()


def detail_value(details: str, name: str) -> str:
    match = re.search(rf"^{re.escape(name)}=(.+)$", details, re.MULTILINE)
    if not match:
        raise SystemExit(f"codesign details are missing {name}")
    return match.group(1).strip()


def require_base_document(
    document: dict[str, Any], platform: str, kind: str
) -> dict[str, Any]:
    if document.get("schema_version") != 1:
        raise SystemExit("unsupported macOS signing evidence schema")
    if document.get("platform") != platform:
        raise SystemExit(
            f"signing evidence platform mismatch: expected {platform}, "
            f"got {document.get('platform')!r}"
        )
    if document.get("artifact_kind") != kind:
        raise SystemExit(
            f"signing evidence kind mismatch: expected {kind}, "
            f"got {document.get('artifact_kind')!r}"
        )
    signing = document.get("codesign")
    notarization = document.get("notarization")
    gatekeeper = document.get("gatekeeper")
    if not isinstance(signing, dict) or signing.get("verified") is not True:
        raise SystemExit("signing evidence does not record strict codesign verification")
    if not str(signing.get("authority", "")).startswith("Developer ID Application:"):
        raise SystemExit("signing evidence does not record a Developer ID Application authority")
    if signing.get("hardened_runtime") is not True:
        raise SystemExit("signing evidence does not record hardened runtime")
    if signing.get("secure_timestamp") is not True:
        raise SystemExit("signing evidence does not record a secure timestamp")
    if not isinstance(notarization, dict) or notarization.get("status") != "Accepted":
        raise SystemExit("signing evidence does not record accepted notarization")
    if not notarization.get("submission_id"):
        raise SystemExit("signing evidence is missing the notarization submission id")
    if not isinstance(gatekeeper, dict) or gatekeeper.get("verified") is not True:
        raise SystemExit("signing evidence does not record Gatekeeper verification")
    return document


def command_write(args: argparse.Namespace) -> None:
    details = args.codesign_details.read_text(encoding="utf-8")
    authority = detail_value(details, "Authority")
    identifier = detail_value(details, "Identifier")
    team_identifier = detail_value(details, "TeamIdentifier")
    if not authority.startswith("Developer ID Application:"):
        raise SystemExit(f"unexpected codesign authority: {authority}")
    if team_identifier in ("", "not set"):
        raise SystemExit("codesign details do not contain a TeamIdentifier")
    flags = detail_value(details, "flags")
    if "runtime" not in flags.lower():
        raise SystemExit("codesign details do not contain hardened runtime flags")
    if not re.search(r"^Timestamp=.+$", details, re.MULTILINE):
        raise SystemExit("codesign details do not contain a secure timestamp")

    submit = read_json(args.notary_submit)
    if submit.get("status") != "Accepted":
        raise SystemExit(
            f"notarization status is not Accepted: {submit.get('status', 'missing')}"
        )
    submission_id = submit.get("id")
    if not isinstance(submission_id, str) or not submission_id:
        raise SystemExit("accepted notarization response is missing an id")
    if not args.gatekeeper_details.read_text(encoding="utf-8").strip():
        raise SystemExit("Gatekeeper verification output is empty")

    document = {
        "artifact_kind": args.kind,
        "artifact_name": args.artifact.name,
        "artifact_sha256": sha256(args.artifact),
        "codesign": {
            "authority": authority,
            "hardened_runtime": True,
            "identifier": identifier,
            "secure_timestamp": True,
            "team_identifier": team_identifier,
            "verified": True,
        },
        "gatekeeper": {"verified": True},
        "notarization": {"status": "Accepted", "submission_id": submission_id},
        "packages": [],
        "platform": args.platform,
        "schema_version": 1,
    }
    write_json(args.output, document)


def command_verify_artifact(args: argparse.Namespace) -> None:
    document = require_base_document(read_json(args.evidence), args.platform, args.kind)
    actual = sha256(args.artifact)
    if document.get("artifact_sha256") != actual:
        raise SystemExit(
            f"signed artifact does not match evidence: expected "
            f"{document.get('artifact_sha256')}, got {actual}"
        )
    if args.checksum:
        expected = read_single_line(args.checksum)
        if expected != actual:
            raise SystemExit(
                f"signed artifact checksum mismatch: expected {expected}, got {actual}"
            )


def command_bind_archive(args: argparse.Namespace) -> None:
    document = require_base_document(read_json(args.evidence), args.platform, "runtime")
    nested_sha = sha256(args.nested_artifact)
    if document.get("artifact_sha256") != nested_sha:
        raise SystemExit(
            "packaged runtime bytes do not match the signed/notarized dylib evidence"
        )
    archive_sha = sha256(args.archive)
    expected_archive_sha = read_single_line(args.checksum)
    if archive_sha != expected_archive_sha:
        raise SystemExit(
            f"runtime archive checksum was not generated from final bytes: "
            f"expected {expected_archive_sha}, got {archive_sha}"
        )
    package = {
        "archive_name": args.archive.name,
        "archive_sha256": archive_sha,
        "nested_artifact_sha256": nested_sha,
        "role": args.role,
    }
    packages = document.setdefault("packages", [])
    if not isinstance(packages, list):
        raise SystemExit("signing evidence packages field is not a list")
    packages[:] = [
        item
        for item in packages
        if not (isinstance(item, dict) and item.get("role") == args.role)
    ]
    packages.append(package)
    packages.sort(key=lambda item: str(item.get("role", "")))
    write_json(args.evidence, document)


def command_verify_archive(args: argparse.Namespace) -> None:
    document = require_base_document(read_json(args.evidence), args.platform, "runtime")
    archive_sha = sha256(args.archive)
    expected_archive_sha = read_single_line(args.checksum)
    if archive_sha != expected_archive_sha:
        raise SystemExit(
            f"runtime archive checksum mismatch: expected {expected_archive_sha}, got {archive_sha}"
        )
    nested_sha = sha256(args.nested_artifact)
    if document.get("artifact_sha256") != nested_sha:
        raise SystemExit("nested runtime dylib does not match signing evidence")
    packages = document.get("packages")
    expected = {
        "archive_name": args.archive.name,
        "archive_sha256": archive_sha,
        "nested_artifact_sha256": nested_sha,
        "role": args.role,
    }
    if not isinstance(packages, list) or expected not in packages:
        raise SystemExit(
            f"signing evidence does not bind the {args.role} runtime archive bytes"
        )


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)

    write = subparsers.add_parser("write")
    write.add_argument("--output", required=True, type=Path)
    write.add_argument("--platform", required=True)
    write.add_argument("--kind", required=True, choices=("cli", "runtime"))
    write.add_argument("--artifact", required=True, type=Path)
    write.add_argument("--codesign-details", required=True, type=Path)
    write.add_argument("--notary-submit", required=True, type=Path)
    write.add_argument("--gatekeeper-details", required=True, type=Path)
    write.set_defaults(handler=command_write)

    verify = subparsers.add_parser("verify-artifact")
    verify.add_argument("--evidence", required=True, type=Path)
    verify.add_argument("--platform", required=True)
    verify.add_argument("--kind", required=True, choices=("cli", "runtime"))
    verify.add_argument("--artifact", required=True, type=Path)
    verify.add_argument("--checksum", type=Path)
    verify.set_defaults(handler=command_verify_artifact)

    bind = subparsers.add_parser("bind-archive")
    bind.add_argument("--evidence", required=True, type=Path)
    bind.add_argument("--platform", required=True)
    bind.add_argument("--archive", required=True, type=Path)
    bind.add_argument("--checksum", required=True, type=Path)
    bind.add_argument("--nested-artifact", required=True, type=Path)
    bind.add_argument("--role", required=True, choices=("builder", "release"))
    bind.set_defaults(handler=command_bind_archive)

    verify_archive = subparsers.add_parser("verify-archive")
    verify_archive.add_argument("--evidence", required=True, type=Path)
    verify_archive.add_argument("--platform", required=True)
    verify_archive.add_argument("--archive", required=True, type=Path)
    verify_archive.add_argument("--checksum", required=True, type=Path)
    verify_archive.add_argument("--nested-artifact", required=True, type=Path)
    verify_archive.add_argument(
        "--role", required=True, choices=("builder", "release")
    )
    verify_archive.set_defaults(handler=command_verify_archive)
    return parser


def main() -> int:
    args = build_parser().parse_args()
    args.handler(args)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
