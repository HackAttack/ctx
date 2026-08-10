#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo 'usage: check-daemon-runtime-dependency-boundary.sh ROOT_BUILD' >&2
  exit 64
fi

root_build="$(readlink -f "$1")"
repo_root="$(dirname "${root_build}")"
tmp="$(mktemp -d "${TEST_TMPDIR:-/tmp}/ctx-daemon-runtime-boundary.XXXXXX")"
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

expected_internal="${tmp}/expected-internal.txt"
printf '%s\n' \
  '//crates/ctx-daemon-runtime:lib' \
  '//crates/ctx-history-core:lib' >"${expected_internal}"
query 'kind("rust_library rule", deps(//crates/ctx-daemon-runtime:lib)) intersect //crates/...' \
  | LC_ALL=C sort -u >"${tmp}/actual-internal.txt"
if ! diff -u "${expected_internal}" "${tmp}/actual-internal.txt"; then
  echo 'unexpected internal dependency closure for ctx-daemon-runtime' >&2
  exit 1
fi

if [[ -z "$(query 'somepath(//crates/ctx-cli:ctx, //crates/ctx-daemon-runtime:lib)')" ]]; then
  echo 'ctx-cli has no Bazel dependency path to ctx-daemon-runtime' >&2
  exit 1
fi

runtime_root="${repo_root}/crates/ctx-daemon-runtime"
actual_internal_cargo="${tmp}/actual-internal-cargo.txt"
grep -E '^[[:space:]]*ctx-[[:alnum:]-]+[[:space:]]*=' "${runtime_root}/Cargo.toml" \
  | sed -E 's/^[[:space:]]*([^[:space:]]+).*/\1/' \
  | LC_ALL=C sort -u >"${actual_internal_cargo}"
printf '%s\n' 'ctx-history-core' >"${tmp}/expected-internal-cargo.txt"
if ! diff -u "${tmp}/expected-internal-cargo.txt" "${actual_internal_cargo}"; then
  echo 'unexpected internal Cargo dependency for ctx-daemon-runtime' >&2
  exit 1
fi

if grep -REn --include='*.rs' \
  'ctx_(history_capture|history_index|history_refresh|pro_host_protocol|semantic_index|semantic_model|upgrade_engine)::|crate::(analytics|output|semantic|ui)::|(^|[^[:alnum:]_])clap::|AppConfig' \
  "${runtime_root}/src"; then
  echo 'product policy or composition dependency leaked into ctx-daemon-runtime' >&2
  exit 1
fi

printf 'ctx-daemon-runtime dependency and composition boundary ok\n'
