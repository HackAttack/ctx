#!/usr/bin/env bash
set -euo pipefail

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# The signing trust boundary is the checked-out source and the narrow launcher.
# This defense-in-depth gate verifies the checked-out bytes inside every
# secrets-capable command. The caller may be local or hosted; Buildkite is not
# a required authority.

head_commit="$(git -C "${root_dir}" rev-parse --verify HEAD)"
git -C "${root_dir}" diff --quiet --ignore-submodules -- || \
  die "tracked source files changed before macOS signing"
git -C "${root_dir}" diff --cached --quiet --ignore-submodules -- || \
  die "staged source files changed before macOS signing"

printf 'macOS signing trust gate ok: clean checkout at %s\n' "${head_commit}"
