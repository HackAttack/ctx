#!/usr/bin/env python3
"""Download the OSV ecosystem snapshots used by the offline release gate."""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import tempfile
from urllib.parse import quote
import urllib.request
import zipfile


UTC = timezone.utc
SOURCES = {
    "crates.io": "https://osv-vulnerabilities.storage.googleapis.com/crates.io/all.zip",
    "npm": "https://osv-vulnerabilities.storage.googleapis.com/npm/all.zip",
}
OBJECTS = {
    "crates.io": "crates.io/all.zip",
    "npm": "npm/all.zip",
}
METADATA_SOURCES = {
    ecosystem: (
        "https://storage.googleapis.com/storage/v1/b/osv-vulnerabilities/o/"
        f"{quote(object_name, safe='')}?fields=generation%2Cupdated&prettyPrint=false"
    )
    for ecosystem, object_name in OBJECTS.items()
}


def latest_source_metadata(ecosystem: str) -> tuple[str, str]:
    request = urllib.request.Request(
        METADATA_SOURCES[ecosystem],
        headers={
            "Cache-Control": "no-cache",
            "User-Agent": "ctx-release-advisory-db/1",
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=120) as response:
            value = json.loads(response.read())
        generation = value["generation"]
        modified = value["updated"]
        if (
            not isinstance(generation, str)
            or not generation.isdecimal()
            or not isinstance(modified, str)
            or not modified.endswith("Z")
        ):
            raise ValueError
        datetime.fromisoformat(modified[:-1] + "+00:00")
    except (KeyError, TypeError, ValueError, json.JSONDecodeError) as error:
        raise SystemExit(f"OSV metadata authority is invalid: {ecosystem}") from error
    return generation, modified


def download(
    ecosystem: str,
    generation: str,
    modified: str,
    destination: Path,
) -> dict[str, object]:
    request = urllib.request.Request(
        f"{SOURCES[ecosystem]}?generation={generation}",
        headers={"User-Agent": "ctx-release-advisory-db/1"},
    )
    destination.parent.mkdir(parents=True, exist_ok=True)
    digest = hashlib.sha256()
    size = 0
    with urllib.request.urlopen(request, timeout=120) as response:
        if response.headers.get("x-goog-generation") != generation:
            raise SystemExit(f"OSV exact-generation response mismatch: {ecosystem}")
        with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as output:
            temporary = Path(output.name)
            while block := response.read(1024 * 1024):
                output.write(block)
                digest.update(block)
                size += len(block)
    try:
        with zipfile.ZipFile(temporary) as archive:
            names = archive.namelist()
            if not names or any(name.startswith("/") or ".." in Path(name).parts for name in names):
                raise SystemExit(f"OSV archive is malformed: {ecosystem}")
            if not any(name.endswith(".json") for name in names):
                raise SystemExit(f"OSV archive contains no advisories: {ecosystem}")
        os.replace(temporary, destination)
    finally:
        temporary.unlink(missing_ok=True)
    return {
        "ecosystem": ecosystem,
        "path": f"osv-scanner/{ecosystem}/all.zip",
        "sha256": digest.hexdigest(),
        "size": size,
        "source_generation": generation,
        "source_last_modified": modified,
        "source_url": SOURCES[ecosystem],
    }


def verify_latest(records: list[dict[str, object]]) -> None:
    for record in records:
        ecosystem = str(record["ecosystem"])
        generation, modified = latest_source_metadata(ecosystem)
        if (
            generation != record["source_generation"]
            or modified != record["source_last_modified"]
        ):
            raise SystemExit(
                f"downloaded OSV generation is no longer current: {ecosystem}"
            )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--database-root", type=Path, required=True)
    parser.add_argument("--metadata", type=Path, required=True)
    parser.add_argument(
        "--ecosystem",
        action="append",
        choices=sorted(SOURCES),
        required=True,
    )
    args = parser.parse_args()
    root = args.database_root.resolve()
    args.metadata.unlink(missing_ok=True)
    ecosystems = sorted(set(args.ecosystem))
    latest = {
        ecosystem: latest_source_metadata(ecosystem) for ecosystem in ecosystems
    }
    records = [
        download(
            ecosystem,
            latest[ecosystem][0],
            latest[ecosystem][1],
            root / f"osv-scanner/{ecosystem}/all.zip",
        )
        for ecosystem in ecosystems
    ]
    verify_latest(records)
    metadata = {
        "schema_version": 2,
        "sealed_at": datetime.now(UTC).isoformat().replace("+00:00", "Z"),
        "databases": records,
    }
    args.metadata.parent.mkdir(parents=True, exist_ok=True)
    payload = json.dumps(metadata, indent=2, sort_keys=True) + "\n"
    with tempfile.NamedTemporaryFile(
        mode="w", encoding="utf-8", dir=args.metadata.parent, delete=False
    ) as output:
        output.write(payload)
        temporary = Path(output.name)
    os.replace(temporary, args.metadata)
    print(args.metadata)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
