#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo 'usage: check-history-read-application-dependency-boundary.sh ROOT_BUILD' >&2
  exit 64
fi

root_build="$(readlink -f "$1")"
repo_root="$(dirname "${root_build}")"
tmp="$(mktemp -d "${TEST_TMPDIR:-/tmp}/ctx-history-read-application-boundary.XXXXXX")"
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
  '//crates/ctx-history-read-application:lib' >"${expected_direct}"
query 'kind("rust_library rule", deps(//crates/ctx-history-read-application:lib, 1)) intersect //crates/...' \
  | LC_ALL=C sort -u >"${tmp}/actual-direct.txt"
if ! diff -u "${expected_direct}" "${tmp}/actual-direct.txt"; then
  echo 'ctx-history-read-application direct Bazel dependency inventory drifted' >&2
  exit 1
fi

manifest="${repo_root}/crates/ctx-history-read-application/Cargo.toml"
sed -n '/^\[dependencies\]$/,/^\[/p' "${manifest}" \
  | grep -E '^[[:space:]]*ctx-[[:alnum:]-]+[[:space:]]*=' \
  | sed -E 's/^[[:space:]]*([^[:space:]]+).*/\1/' \
  | LC_ALL=C sort -u >"${tmp}/actual-cargo.txt"
printf '%s\n' \
  'ctx-history-core' \
  'ctx-history-index-format' \
  'ctx-history-index-query' >"${tmp}/expected-cargo.txt"
if ! diff -u "${tmp}/expected-cargo.txt" "${tmp}/actual-cargo.txt"; then
  echo 'ctx-history-read-application direct Cargo dependency inventory drifted' >&2
  exit 1
fi

for forbidden in \
  '//crates/ctx-history-capture:lib' \
  '//crates/ctx-history-refresh:lib' \
  '//crates/ctx-semantic-index:lib' \
  '//crates/ctx-cli:ctx'; do
  if [[ -n "$(query "somepath(//crates/ctx-history-read-application:lib, ${forbidden})")" ]]; then
    echo "ctx-history-read-application has forbidden Bazel dependency path to ${forbidden}" >&2
    exit 1
  fi
done
if [[ -z "$(query 'somepath(//crates/ctx-cli:ctx, //crates/ctx-history-read-application:lib)')" ]]; then
  echo 'ctx-cli has no Bazel dependency path to ctx-history-read-application' >&2
  exit 1
fi

query_root="${repo_root}/crates/ctx-history-read-application"
if grep -En 'ctx-(history-capture|history-refresh|semantic-index|cli)|clap' "${manifest}"; then
  echo 'forbidden runtime, writer, or transport dependency in ctx-history-read-application' >&2
  exit 1
fi
if grep -REn --include='*.rs' \
  'ctx_history_(capture|refresh)::|ctx_semantic_index::|crate::(config|daemon|output|ui)::' \
  "${query_root}/src"; then
  echo 'forbidden source dependency in ctx-history-read-application' >&2
  exit 1
fi
if grep -REn --include='*.rs' \
  'std::env|std::process|process::Command|Command::new|CODEX_THREAD_ID|CaptureProvider::Codex' \
  "${query_root}/src"; then
  echo 'environment, process, or caller identity leaked into ctx-history-read-application' >&2
  exit 1
fi
if grep -REn --include='*.rs' \
  'SearchRefreshMode|RefreshArg|semantic_daemon|DaemonConfig|SearchConfig|SourceBackedRefresh' \
  "${query_root}/src"; then
  echo 'daemon, configuration, or refresh lifecycle interpretation leaked into ctx-history-read-application' >&2
  exit 1
fi
if grep -REn --include='*.rs' \
  'clap::|shell_quote|--[[:alnum:]][[:alnum:]-]*|ctx (search|show|setup|doctor)' \
  "${query_root}/src"; then
  echo 'transport or presentation behavior leaked into ctx-history-read-application' >&2
  exit 1
fi
if grep -REn --include='*.rs' \
  '#\[path[[:space:]]*=|include!|include_str!|include_bytes!' \
  "${query_root}/src"; then
  echo 'ctx-history-read-application source must remain package-local' >&2
  exit 1
fi
if grep -REn --include='*.rs' \
  'dyn[[:space:]]+(GenerationReadPort|HistorySemanticPort)' \
  "${query_root}/src"; then
  echo 'history read application ports must use static dispatch' >&2
  exit 1
fi
if [[ "$(grep -REh --include='*.rs' '^pub fn execute_search<' "${query_root}/src" | wc -l)" -ne 1 ]] \
  || [[ "$(grep -REh --include='*.rs' '^pub fn execute_locate<' "${query_root}/src" | wc -l)" -ne 1 ]]; then
  echo 'search and locate must each have one production application authority' >&2
  exit 1
fi
if grep -Eq 'PinnedHistoryQuery|\.search\(' \
  "${repo_root}/crates/ctx-cli/src/commands/source_index/search.rs" \
  || grep -Eq 'PinnedHistoryQuery|\.locate\(' \
    "${repo_root}/crates/ctx-cli/src/commands/source_index/locate.rs"; then
  echo 'ctx-cli bypasses the application-owned search or locate authority' >&2
  exit 1
fi
if [[ -e "${repo_root}/crates/ctx-history-query" ]]; then
  echo 'legacy ctx-history-query production authority still exists' >&2
  exit 1
fi
if grep -REn --include='*.rs' \
  '/home/[[:alnum:]_.-]+|/Users/[[:alnum:]_.-]+|ctx-private|ctx-multi-repo-workspace|\.ctx/worktrees' \
  "${query_root}/src"; then
  echo 'private host or workspace path leaked into ctx-history-read-application' >&2
  exit 1
fi
if grep -REn --include='*.rs' \
  '[Ww]ork [Rr]ecorder|ctx publish|ctx evidence|ctx link-pr|ctx context|ctx uninstall|auto[_-]update|CTX_UPDATE|provider-live|completion-certificate|dashboard export|upsert_github|write[_-]shim' \
  "${query_root}/src"; then
  echo 'retired product or legacy control surface leaked into ctx-history-read-application' >&2
  exit 1
fi

physical_lines="$(find "${query_root}/src" -type f -name '*.rs' -print0 \
  | xargs -0 awk 'END { print NR }')"
expected_physical_lines=6425
if (( physical_lines >= 20000 )); then
  echo "ctx-history-read-application reached its 20,000-line hard stop: ${physical_lines}" >&2
  exit 1
fi
if (( physical_lines != expected_physical_lines )); then
  echo "ctx-history-read-application physical-line ratchet drifted: expected ${expected_physical_lines}, found ${physical_lines}" >&2
  exit 1
fi
