#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${TEST_SRCDIR:-}" && -n "${TEST_WORKSPACE:-}" ]]; then
  source_root="${TEST_SRCDIR}/${TEST_WORKSPACE}"
else
  source_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
fi
tmp="$(mktemp -d "${TMPDIR:-/tmp}/ctx-authenticated-semantic-receipt.XXXXXX")"
trap 'rm -rf "${tmp}"' EXIT
repo="${tmp}/repo"
mkdir -p "${repo}/scripts/buildkite" "${tmp}/bin"
cp -L "${source_root}/scripts/buildkite/download-authenticated-semantic-receipt.sh" \
  "${repo}/scripts/buildkite/download-authenticated-semantic-receipt.sh"
chmod 0755 "${repo}/scripts/buildkite/download-authenticated-semantic-receipt.sh"
helper="${repo}/scripts/buildkite/download-authenticated-semantic-receipt.sh"
build_id='11111111-1111-4111-8111-111111111111'
job_id='aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa'
artifact_path='target/public-cli-semantic-native-smoke/macos-arm64/ctx-macos-arm64.semantic-execution.json'
fixture='authenticated semantic receipt fixture'
fixture_sha="$(printf '%s\n' "${fixture}" | sha256sum | awk '{print $1}')"
log="${tmp}/buildkite-agent.log"

cat > "${tmp}/bin/buildkite-agent" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%q ' "$@" >> "${CTX_TEST_BUILDKITE_LOG}"
printf '\n' >> "${CTX_TEST_BUILDKITE_LOG}"
command="$1"
subcommand="$2"
shift 2
[[ "${command}:${subcommand}" == "artifact:search" \
  || "${command}:${subcommand}" == "artifact:download" ]]
query="$1"
shift
if [[ "${subcommand}" == "search" ]]; then
  [[ " $* " == *" --step github-macos-arm64-semantic-native-smoke "* ]]
  [[ " $* " == *" --build ${BUILDKITE_BUILD_ID} "* ]]
  [[ " $* " != *" --include-retried-jobs "* ]]
  if [[ "${CTX_TEST_DUPLICATE_RESULTS:-0}" == "1" ]]; then
    printf '%s\t%s\t%s\n' "${CTX_TEST_JOB_ID}" "${query}" "${CTX_TEST_ARTIFACT_SHA256}"
  fi
  printf '%s\t%s\t%s\n' "${CTX_TEST_JOB_ID}" "${query}" "${CTX_TEST_ARTIFACT_SHA256}"
else
  destination="$1"
  shift
  [[ "${destination}" == "." ]]
  [[ " $* " == *" --step ${CTX_TEST_JOB_ID} "* ]]
  [[ " $* " == *" --build ${BUILDKITE_BUILD_ID} "* ]]
  mkdir -p "$(dirname "${query}")"
  printf '%s\n' "${CTX_TEST_RECEIPT_FIXTURE}" > "${query}"
fi
SH
chmod 0755 "${tmp}/bin/buildkite-agent"
export PATH="${tmp}/bin:${PATH}"
export CTX_TEST_BUILDKITE_LOG="${log}"
export CTX_TEST_JOB_ID="${job_id}"
export CTX_TEST_ARTIFACT_SHA256="${fixture_sha}"
export CTX_TEST_RECEIPT_FIXTURE="${fixture}"

BUILDKITE_BUILD_ID="${build_id}" BUILDKITE_BUILD_NUMBER=42 \
  "${helper}" macos-arm64 > "${tmp}/positive.out"
receipt="${repo}/${artifact_path}"
authority="${receipt%.json}.artifact-authority.json"
test "$(cat "${receipt}")" = "${fixture}"
python3 -I - "${authority}" "${build_id}" "${job_id}" "${fixture_sha}" <<'PY'
import json
import sys

path, build_id, job_id, digest = sys.argv[1:]
with open(path, encoding="utf-8") as source:
    value = json.load(source)
expected = {
    "artifact_path": "target/public-cli-semantic-native-smoke/macos-arm64/ctx-macos-arm64.semantic-execution.json",
    "artifact_sha256": digest,
    "attempt_selection": "latest",
    "build_id": build_id,
    "build_number": 42,
    "kind": "ctx-buildkite-semantic-receipt-artifact-authority",
    "producer_job_id": job_id,
    "producer_step_key": "github-macos-arm64-semantic-native-smoke",
    "schema_version": 1,
}
if value != expected:
    raise SystemExit(f"unexpected artifact authority: {value!r}")
PY
grep -Fq -- "artifact search ${artifact_path}" "${log}"
grep -Fq -- "artifact download ${artifact_path} . --step ${job_id}" "${log}"

rm -rf "${repo}/target"
export CTX_TEST_ARTIFACT_SHA256="$(printf '0%.0s' {1..64})"
if BUILDKITE_BUILD_ID="${build_id}" BUILDKITE_BUILD_NUMBER=42 \
  "${helper}" macos-arm64 > "${tmp}/bad-sha.out" 2> "${tmp}/bad-sha.err"; then
  printf 'authenticated receipt helper accepted an API SHA mismatch\n' >&2
  exit 1
fi
grep -Fq 'does not match Buildkite artifact SHA-256' "${tmp}/bad-sha.err"
test ! -e "${repo}/target/public-cli-semantic-native-smoke/macos-arm64/ctx-macos-arm64.semantic-execution.artifact-authority.json"

rm -rf "${repo}/target"
export CTX_TEST_ARTIFACT_SHA256="${fixture_sha}"
export CTX_TEST_DUPLICATE_RESULTS=1
if BUILDKITE_BUILD_ID="${build_id}" BUILDKITE_BUILD_NUMBER=42 \
  "${helper}" macos-arm64 > "${tmp}/duplicate.out" 2> "${tmp}/duplicate.err"; then
  printf 'authenticated receipt helper accepted multiple producer attempts\n' >&2
  exit 1
fi
grep -Fq 'expected exactly one latest-attempt macos-arm64 semantic receipt artifact' \
  "${tmp}/duplicate.err"
test ! -e "${repo}/${artifact_path}"
test ! -e "${repo}/target/public-cli-semantic-native-smoke/macos-arm64/ctx-macos-arm64.semantic-execution.artifact-authority.json"

printf 'authenticated Buildkite semantic receipt tests passed\n'
