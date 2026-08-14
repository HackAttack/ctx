#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo 'usage: check-history-provider-sqlite-logical-boundary.sh ROOT_BUILD' >&2
  exit 64
fi

root_build="$(readlink -f "$1")"
repo_root="$(dirname "${root_build}")"
scratch="$(mktemp -d "${TEST_TMPDIR:-/tmp}/ctx-history-provider-sqlite-logical-boundary.XXXXXX")"
trap 'rm -rf -- "${scratch}"' EXIT
mkdir -p "${scratch}/home"

query() {
  env -u BUILD_WORKSPACE_DIRECTORY \
    HOME="${scratch}/home" \
    BAZEL_OUTPUT_USER_ROOT="${scratch}/bazel-output" \
    CTX_BAZEL_SANDBOX_BASE="${scratch}/bazel-sandboxes" \
    CTX_BAZEL_WORKSPACE="${repo_root}" \
    "${repo_root}/scripts/bazelw" query "$1" --output=label
}

target='//crates/ctx-history-providers-sqlite-logical:lib'
query "kind(\"rust_library rule\", deps(${target}, 1)) intersect //crates/..." \
  | LC_ALL=C sort -u >"${scratch}/direct-labels.txt"
query "kind(\"rust_library rule\", deps(${target})) intersect //crates/..." \
  | LC_ALL=C sort -u >"${scratch}/closure-labels.txt"

python3 "${repo_root}/tools/bazel/check_history_provider_sqlite_logical_boundary.py" \
  "${repo_root}/crates/ctx-history-providers-sqlite-logical/Cargo.toml" \
  "${repo_root}/crates/ctx-history-providers-sqlite-logical/src" \
  "${repo_root}/crates/ctx-history-capture/Cargo.toml" \
  "${repo_root}/crates/ctx-history-capture/src" \
  "${repo_root}/crates/ctx-history-capture-runtime/src" \
  "${repo_root}/crates/ctx-history-source-discovery/src" \
  "${scratch}/direct-labels.txt" \
  "${scratch}/closure-labels.txt"
