#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
smoke="${repo_root}/scripts/run-native-candidate-smoke.sh"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/ctx-native-smoke-test.XXXXXX")"
trap 'rm -rf "${tmp}"' EXIT

fake="${tmp}/ctx"
cat > "${fake}" <<'EOF'
#!/bin/sh
set -eu

test "${CTX_ANALYTICS_OFF:-}" = 1
test "${CTX_UPGRADE_OFF:-}" = 1
test "${CTX_DAEMON_AUTOSTART_OFF:-}" = 1
test -n "${CTX_DATA_ROOT:-}"
test -n "${HOME:-}"
test -n "${XDG_CONFIG_HOME:-}"
test -n "${XDG_CACHE_HOME:-}"
test "${HOME}" != "${ORIGINAL_HOME:-not-in-clean-env}"

case " $* " in
  *" --backend semantic "*)
    test "${CTX_SEARCH_SEMANTIC:-}" = 1
    test "${CTX_DAEMON_ENABLED:-}" = 1
    printf '%s\n' 'local semantic search is not supported on this platform yet' >&2
    exit 1
    ;;
  *)
    test "${CTX_DISABLE_DAEMON:-}" = 1
    test "${CTX_SEARCH_SEMANTIC:-}" = 0
    ;;
esac

case "${1:-}" in
  --version)
    version=0.25.0
    case "$0" in *bad-version*) version=9.9.9 ;; esac
    printf 'ctx %s\n' "${version}"
    ;;
  setup)
    ;;
  import)
    printf '%s\n' '{"totals":{"imported_events":2}}'
    ;;
  search)
    printf '%s\n' '{"retrieval":{"requested_mode":"lexical","effective_mode":"lexical"},"results":[{"text":"Add a parser test."}]}'
    ;;
  status)
    printf '%s\n' '{"read_only":true,"semantic":{"embed_policy":{"source":"unsupported"}}}'
    ;;
  *)
    printf 'unexpected fake ctx arguments: %s\n' "$*" >&2
    exit 1
    ;;
esac
EOF
chmod +x "${fake}"
printf '%s\n' '{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}' > "${tmp}/fixture.jsonl"

result="${tmp}/result.json"
"${smoke}" "${fake}" "${tmp}/fixture.jsonl" 0.25.0 "${result}" >/dev/null
expected='{"schema_version":1,"kind":"ctx-native-candidate-smoke","status":"passed","steps":{"version":"passed","setup":"passed","import":"passed","search":"passed","read_only":"passed","capability":"passed"}}'
[[ "$(tr -d '\r\n' < "${result}")" == "${expected}" ]] || {
  printf 'candidate smoke result schema changed\n' >&2
  cat "${result}" >&2
  exit 1
}

# Exercise the non-FastEmbed policy branch deterministically. The private proof
# still derives the real guest OS independently; this fake uname exists only in
# this focused helper test.
mkdir -p "${tmp}/fake-path"
cat > "${tmp}/fake-path/uname" <<'EOF'
#!/bin/sh
case "${1:-}" in
  -s) printf 'FreeBSD\n' ;;
  -m) printf 'amd64\n' ;;
  *) exec /usr/bin/uname "$@" ;;
esac
EOF
chmod +x "${tmp}/fake-path/uname"
capability_result="${tmp}/capability-result.json"
PATH="${tmp}/fake-path:${PATH}" "${smoke}" \
  "${fake}" "${tmp}/fixture.jsonl" 0.25.0 "${capability_result}" >/dev/null
[[ "$(tr -d '\r\n' < "${capability_result}")" == "${expected}" ]]

failed_result="${tmp}/failed-result.json"
cp "${fake}" "${tmp}/ctx-bad-version"
if "${smoke}" \
  "${tmp}/ctx-bad-version" "${tmp}/fixture.jsonl" 0.25.0 "${failed_result}" \
  >"${tmp}/failure.out" 2>"${tmp}/failure.err"; then
  printf 'candidate smoke accepted a mismatched version\n' >&2
  exit 1
fi
[[ ! -e "${failed_result}" ]] || {
  printf 'candidate smoke wrote passing evidence after failure\n' >&2
  exit 1
}
grep -Fq 'candidate version mismatch' "${tmp}/failure.err"

hung_result="${tmp}/hung-result.json"
cp "${fake}" "${tmp}/ctx-hang"
sed -i '/case "${1:-}" in/i\case "$0" in *ctx-hang) sleep 30 ;; esac' "${tmp}/ctx-hang"
started="$(date +%s)"
if CTX_NATIVE_CANDIDATE_COMMAND_TIMEOUT_SECONDS=1 "${smoke}" \
  "${tmp}/ctx-hang" "${tmp}/fixture.jsonl" 0.25.0 "${hung_result}" \
  >"${tmp}/hung.out" 2>"${tmp}/hung.err"; then
  printf 'candidate smoke accepted a hung command\n' >&2
  exit 1
fi
elapsed="$(( $(date +%s) - started ))"
[[ "${elapsed}" -lt 10 ]] || {
  printf 'candidate smoke timeout was not bounded: %ss\n' "${elapsed}" >&2
  exit 1
}
[[ ! -e "${hung_result}" ]]
grep -Fq 'candidate command exceeded 1 seconds' "${tmp}/hung.err"

printf 'native candidate smoke tests passed\n'
