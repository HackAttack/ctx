#!/usr/bin/env python3
"""Resolve one exact cargo-zigbuild executable before a factory build."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--expected-version", required=True)
    args = parser.parse_args()
    selected = shutil.which("cargo-zigbuild")
    if selected is None:
        raise SystemExit("cargo-zigbuild is not present on PATH")
    path = Path(os.path.realpath(selected))
    metadata = path.stat()
    if not stat.S_ISREG(metadata.st_mode) or not os.access(path, os.X_OK):
        raise SystemExit("cargo-zigbuild must resolve to a regular executable")
    observed = subprocess.run(
        [os.fspath(path), "--version"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    expected = f"cargo-zigbuild {args.expected_version}"
    if observed != expected:
        raise SystemExit(
            f"cargo-zigbuild version mismatch: expected {expected!r}, got {observed!r}"
        )
    print(
        json.dumps(
            {
                "path": os.fspath(path),
                "observed_version": observed.removeprefix("cargo-zigbuild "),
            }
        )
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, subprocess.CalledProcessError) as error:
        raise SystemExit(f"could not validate cargo-zigbuild: {error}") from error
