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

workspace_members="${tmp}/workspace-members.txt"
sed -n '/^members = \[/,/^\]/p' "${repo_root}/Cargo.toml" \
  | sed -nE 's/^[[:space:]]*"(crates\/[^"[:space:]]+)",?[[:space:]]*$/\1/p' \
  | LC_ALL=C sort -u >"${workspace_members}"
if [[ ! -s "${workspace_members}" ]]; then
  echo 'Cargo workspace crate inventory is empty or unreadable' >&2
  exit 1
fi

visible_manifests="${tmp}/visible-manifests.txt"
find "${repo_root}/crates" -mindepth 2 -maxdepth 2 -name Cargo.toml -type f -printf '%h\n' \
  | sed "s#^${repo_root}/##" \
  | LC_ALL=C sort -u >"${visible_manifests}"
if ! diff -u "${workspace_members}" "${visible_manifests}"; then
  echo 'boundary runfiles do not expose the complete Cargo workspace crate inventory' >&2
  exit 1
fi
while IFS= read -r member; do
  if [[ ! -f "${repo_root}/${member}/BUILD.bazel" ]]; then
    echo "boundary runfiles omit the Bazel BUILD graph for ${member}" >&2
    exit 1
  fi
done <"${workspace_members}"

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

expected_reverse_bazel="${tmp}/expected-reverse-bazel.txt"
# The fixture binaries compile the CLI production source under controlled cfg
# variants, so the complete binary/library reverse set is intentionally exact.
printf '%s\n' \
  '//crates/ctx-cli:ctx' \
  '//crates/ctx-cli:ctx_auto_upgrade_acceptance_fixture' \
  '//crates/ctx-cli:ctx_pro_test_host' \
  '//crates/ctx-cli:ctx_upgrade_test_harness' \
  '//crates/ctx-daemon-runtime:lib' >"${expected_reverse_bazel}"
query 'kind("rust_binary rule", rdeps(//crates/..., //crates/ctx-daemon-runtime:lib)) union kind("rust_library rule", rdeps(//crates/..., //crates/ctx-daemon-runtime:lib))' \
  | LC_ALL=C sort -u >"${tmp}/actual-reverse-bazel.txt"
if ! diff -u "${expected_reverse_bazel}" "${tmp}/actual-reverse-bazel.txt"; then
  echo 'unexpected reverse production consumer of ctx-daemon-runtime' >&2
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

actual_reverse_cargo="${tmp}/actual-reverse-cargo.txt"
while IFS= read -r manifest; do
  if [[ "${manifest}" != "${runtime_root}/Cargo.toml" ]] && grep -q 'ctx-daemon-runtime' "${manifest}"; then
    printf '%s\n' "${manifest#${repo_root}/}"
  fi
done < <(find "${repo_root}/crates" -mindepth 2 -maxdepth 2 -name Cargo.toml -type f | LC_ALL=C sort) \
  >"${actual_reverse_cargo}"
printf '%s\n' 'crates/ctx-cli/Cargo.toml' >"${tmp}/expected-reverse-cargo.txt"
if ! diff -u "${tmp}/expected-reverse-cargo.txt" "${actual_reverse_cargo}"; then
  echo 'unexpected reverse Cargo consumer of ctx-daemon-runtime' >&2
  exit 1
fi

if grep -REn --include='*.rs' \
  'ctx_(history_capture|history_index|history_refresh|pro_host_protocol|semantic_index|semantic_model|upgrade_engine)::|crate::(analytics|output|semantic|ui)::|(^|[^[:alnum:]_])clap::|AppConfig' \
  "${runtime_root}/src"; then
  echo 'product policy or composition dependency leaked into ctx-daemon-runtime' >&2
  exit 1
fi

printf 'ctx-daemon-runtime dependency and composition boundary ok: workspace_crates=%s\n' \
  "$(wc -l <"${workspace_members}")"
