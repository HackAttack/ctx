#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo 'usage: check-history-provider-pack-boundary.sh ROOT_BUILD' >&2
  exit 64
fi

root_build="$(readlink -f "$1")"
repo_root="$(dirname "${root_build}")"

exec python3 "$repo_root/tools/bazel/check_history_provider_pack_boundary.py" \
  "$repo_root/Cargo.toml" \
  "$repo_root/crates/ctx-history-providers-task-docs/Cargo.toml" \
  "$repo_root/crates/ctx-history-providers-task-docs/BUILD.bazel" \
  "$repo_root/crates/ctx-history-capture-composition/Cargo.toml" \
  "$repo_root/crates/ctx-history-capture-composition/BUILD.bazel" \
  "$repo_root"/crates/*/Cargo.toml \
  --member-builds \
  "$repo_root/BUILD.bazel" \
  "$repo_root"/crates/*/BUILD.bazel
