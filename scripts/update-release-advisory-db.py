#!/usr/bin/env python3
"""Download the OSV ecosystem snapshots used by the offline release gate."""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
from email.utils import parsedate_to_datetime
import hashlib
import json
import os
from pathlib import Path
import tempfile
from typing import Any
import urllib.request
import zipfile


UTC = timezone.utc
SOURCES = {
    "crates.io": "https://osv-vulnerabilities.storage.googleapis.com/crates.io/all.zip",
    "npm": "https://osv-vulnerabilities.storage.googleapis.com/npm/all.zip",
}


def response_source_metadata(ecosystem: str, response: Any) -> tuple[str, str]:
    modified_header = response.headers.get("Last-Modified")
    generation = response.headers.get("x-goog-generation")
    if not modified_header or not generation:
        raise SystemExit(f"OSV response lacks source metadata: {ecosystem}")
    modified = parsedate_to_datetime(modified_header).astimezone(UTC)
    return generation, modified.isoformat().replace("+00:00", "Z")


def download(ecosystem: str, destination: Path) -> dict[str, object]:
    request = urllib.request.Request(
        SOURCES[ecosystem], headers={"User-Agent": "ctx-release-advisory-db/1"}
    )
    destination.parent.mkdir(parents=True, exist_ok=True)
    digest = hashlib.sha256()
    size = 0
    with urllib.request.urlopen(request, timeout=120) as response:
        generation, modified = response_source_metadata(ecosystem, response)
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
        request = urllib.request.Request(
            SOURCES[ecosystem],
            headers={"User-Agent": "ctx-release-advisory-db/1"},
            method="HEAD",
        )
        with urllib.request.urlopen(request, timeout=120) as response:
            generation, modified = response_source_metadata(ecosystem, response)
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
    records = [
        download(ecosystem, root / f"osv-scanner/{ecosystem}/all.zip")
        for ecosystem in sorted(set(args.ecosystem))
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
