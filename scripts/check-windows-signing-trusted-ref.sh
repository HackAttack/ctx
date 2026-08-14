#!/usr/bin/env bash
set -euo pipefail

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
head_commit="$(git -C "${root_dir}" rev-parse --verify HEAD)"
git -C "${root_dir}" diff --quiet --ignore-submodules -- || \
  die "tracked source files changed before Windows signing"
git -C "${root_dir}" diff --cached --quiet --ignore-submodules -- || \
  die "staged source files changed before Windows signing"

printf 'Windows signing trust gate ok: clean checkout at %s\n' "${head_commit}"
