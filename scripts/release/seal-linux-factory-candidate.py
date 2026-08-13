#!/usr/bin/env python3
"""Seal the two Linux sub-bundles inside a shared five-target factory output."""

from __future__ import annotations

import argparse
import importlib.util
from pathlib import Path
import shutil
import tempfile


def load_release_bundle():
    path = Path(__file__).resolve().with_name("release_bundle.py")
    spec = importlib.util.spec_from_file_location("ctx_release_bundle", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"could not load release bundle module: {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


release_bundle = load_release_bundle()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--candidate-dir", required=True, type=Path)
    parser.add_argument("--source-commit", required=True)
    args = parser.parse_args()
    candidate = args.candidate_dir.resolve(strict=True)
    for platform in ("linux-x64", "linux-aarch64"):
        with tempfile.TemporaryDirectory(
            prefix=f"ctx-{platform}-seal-", dir=candidate.parent
        ) as temporary:
            stage = Path(temporary)
            for name in release_bundle.expected_release_leaves(platform):
                shutil.copy2(candidate / name, stage / name)
            release_bundle.seal_bundle(stage, platform, args.source_commit)
            marker = stage / f"ctx-{platform}.release-complete.json"
            shutil.copy2(marker, candidate / marker.name)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
