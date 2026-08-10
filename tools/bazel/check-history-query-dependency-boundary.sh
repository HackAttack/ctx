#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo 'usage: check-history-query-dependency-boundary.sh ROOT_BUILD' >&2
  exit 64
fi

root_build="$(readlink -f "$1")"
repo_root="$(dirname "${root_build}")"
tmp="$(mktemp -d "${TEST_TMPDIR:-/tmp}/ctx-history-query-boundary.XXXXXX")"
trap 'rm -rf -- "${tmp}"' EXIT
mkdir -p "${tmp}/home"

query() {
  env -u BUILD_WORKSPACE_DIRECTORY \
    HOME="${tmp}/home" \
    BAZEL_OUTPUT_USER_ROOT="${tmp}/bazel-output" \
    CTX_BAZEL_SANDBOX_BASE="${tmp}/bazel-sandboxes" \
    CTX_BAZEL_WORKSPACE="${repo_root}" \
    "${repo_root}/scripts/bazelw" query "$1" --output=label
}

expected_direct="${tmp}/expected-direct.txt"
printf '%s\n' \
  '//crates/ctx-history-core:lib' \
  '//crates/ctx-history-index-format:lib' \
  '//crates/ctx-history-index-query:lib' \
  '//crates/ctx-history-query:lib' >"${expected_direct}"
query 'kind("rust_library rule", deps(//crates/ctx-history-query:lib, 1)) intersect //crates/...' \
  | LC_ALL=C sort -u >"${tmp}/actual-direct.txt"
if ! diff -u "${expected_direct}" "${tmp}/actual-direct.txt"; then
  echo 'ctx-history-query direct Bazel dependency inventory drifted' >&2
  exit 1
fi

manifest="${repo_root}/crates/ctx-history-query/Cargo.toml"
sed -n '/^\[dependencies\]$/,/^\[/p' "${manifest}" \
  | grep -E '^[[:space:]]*ctx-[[:alnum:]-]+[[:space:]]*=' \
  | sed -E 's/^[[:space:]]*([^[:space:]]+).*/\1/' \
  | LC_ALL=C sort -u >"${tmp}/actual-cargo.txt"
printf '%s\n' \
  'ctx-history-core' \
  'ctx-history-index-format' \
  'ctx-history-index-query' >"${tmp}/expected-cargo.txt"
if ! diff -u "${tmp}/expected-cargo.txt" "${tmp}/actual-cargo.txt"; then
  echo 'ctx-history-query direct Cargo dependency inventory drifted' >&2
  exit 1
fi

for forbidden in \
  '//crates/ctx-history-capture:lib' \
  '//crates/ctx-history-refresh:lib' \
  '//crates/ctx-semantic-index:lib' \
  '//crates/ctx-cli:ctx'; do
  if [[ -n "$(query "somepath(//crates/ctx-history-query:lib, ${forbidden})")" ]]; then
    echo "ctx-history-query has forbidden Bazel dependency path to ${forbidden}" >&2
    exit 1
  fi
done
if [[ -z "$(query 'somepath(//crates/ctx-cli:ctx, //crates/ctx-history-query:lib)')" ]]; then
  echo 'ctx-cli has no Bazel dependency path to ctx-history-query' >&2
  exit 1
fi

query_root="${repo_root}/crates/ctx-history-query"
if grep -En 'ctx-(history-capture|history-refresh|semantic-index|cli)|clap' "${manifest}"; then
  echo 'forbidden runtime, writer, or transport dependency in ctx-history-query' >&2
  exit 1
fi
if grep -REn --include='*.rs' \
  'ctx_history_(capture|refresh)::|ctx_semantic_index::|crate::(config|daemon|output|ui)::' \
  "${query_root}/src"; then
  echo 'forbidden source dependency in ctx-history-query' >&2
  exit 1
fi

physical_lines="$(find "${query_root}/src" -type f -name '*.rs' -print0 \
  | xargs -0 awk 'END { print NR }')"
if (( physical_lines > 17500 )); then
  echo "ctx-history-query exceeds its 17,500-line extraction target: ${physical_lines}" >&2
  exit 1
fi
