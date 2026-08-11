#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo 'usage: check-history-cli-boundary.sh ROOT_BUILD' >&2
  exit 64
fi

root_build="$(readlink -f "$1")"
repo_root="$(dirname "${root_build}")"

exec python3 "$repo_root/tools/bazel/check_history_cli_boundary.py" \
  "$repo_root/Cargo.toml" \
  "$repo_root/crates/ctx-history-cli/Cargo.toml" \
  "$repo_root/crates/ctx-history-cli/BUILD.bazel" \
  "$repo_root/crates/ctx-cli/Cargo.toml" \
  "$repo_root/crates/ctx-cli/BUILD.bazel" \
  "$repo_root/crates/ctx-history-cli/src" \
  "$repo_root/crates/ctx-cli/src/provider_args.rs" \
  "$repo_root/crates/ctx-cli/src/provider_sources.rs" \
  "$repo_root"/crates/*/Cargo.toml \
  --member-builds \
  "$repo_root/BUILD.bazel" \
  "$repo_root"/crates/*/BUILD.bazel
