#!/usr/bin/env python3

import hashlib
import json
import os
from pathlib import Path
import sys


config_path = Path(__file__).with_suffix(".config.json")
config = json.loads(config_path.read_text(encoding="utf-8"))
with Path(config["environment_receipt"]).open("a", encoding="utf-8") as receipt:
    receipt.write(
        json.dumps(
            {
                "arguments": sys.argv[1:],
                "environment": dict(os.environ),
            },
            sort_keys=True,
        )
        + "\n"
    )

if sys.argv[1:] == ["--version"]:
    replacement = config.get("replace_after_validation")
    if replacement is not None:
        Path(replacement["archive"]).write_bytes(b"replacement database\n")
        Path(replacement["metadata"]).write_text(
            "replacement metadata\n", encoding="utf-8"
        )
    print("osv-scanner version: 2.4.0")
    print("osv-scalibr version: fixture")
    raise SystemExit(0)

exit_code = int(config["exit_code"])
if exit_code not in {0, 1}:
    raise SystemExit(exit_code)
database_receipt = config.get("database_receipt")
if database_receipt is not None:
    database_root = Path(os.environ["OSV_SCANNER_LOCAL_DB_CACHE_DIRECTORY"])
    database = database_root / "osv-scanner/crates.io/all.zip"
    Path(database_receipt).write_text(
        json.dumps(
            {
                "root": str(database_root),
                "sha256": hashlib.sha256(database.read_bytes()).hexdigest(),
            }
        ),
        encoding="utf-8",
    )
fixture = Path(config["fixture"])
value = json.loads(fixture.read_text(encoding="utf-8"))
lockfiles = [
    str(Path(sys.argv[index + 1]).resolve())
    for index, argument in enumerate(sys.argv)
    if argument == "-L"
]
if len(lockfiles) != len(value["results"]):
    raise SystemExit(64)
for result, lockfile in zip(value["results"], lockfiles, strict=True):
    result["source"]["path"] = lockfile
print(json.dumps(value))
raise SystemExit(exit_code)
