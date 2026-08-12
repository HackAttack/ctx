#!/usr/bin/env python3

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
    print("osv-scanner version: 2.4.0")
    print("osv-scalibr version: fixture")
    raise SystemExit(0)

exit_code = int(config["exit_code"])
if exit_code not in {0, 1}:
    raise SystemExit(exit_code)
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
