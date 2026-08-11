#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo 'usage: check-history-source-discovery-boundary.sh ROOT_BUILD' >&2
  exit 64
fi

root_build="$(readlink -f "$1")"
repo_root="$(dirname "${root_build}")"
scratch="$(mktemp -d "${TEST_TMPDIR:-/tmp}/ctx-history-source-discovery-boundary.XXXXXX")"
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

target='//crates/ctx-history-source-discovery:lib'
query "kind(\"rust_library rule\", deps(${target}, 1)) intersect //crates/..." \
  | LC_ALL=C sort -u >"${scratch}/direct-labels.txt"
query "kind(\"rust_library rule\", deps(${target})) intersect //crates/..." \
  | LC_ALL=C sort -u >"${scratch}/closure-labels.txt"

python3 "${repo_root}/tools/bazel/check_history_source_discovery_boundary.py" \
  "${repo_root}/crates/ctx-history-source-discovery/Cargo.toml" \
  "${scratch}/direct-labels.txt" \
  "${scratch}/closure-labels.txt"

source_root="${repo_root}/crates/ctx-history-source-discovery/src"
if grep -REn --include='*.rs' \
  'ctx_history_(capture|provider|runtime|daemon|refresh|refresh_execution|index_format|index_generation|index_query|jsonl|repository_evidence)::' \
  "${source_root}"; then
  echo 'forbidden production authority reference in ctx-history-source-discovery' >&2
  exit 1
fi
