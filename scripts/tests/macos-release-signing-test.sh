#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
test_root="$(mktemp -d "${TMPDIR:-/tmp}/ctx-macos-signing-test.XXXXXX")"
trap 'rm -rf "${test_root}"' EXIT
fake_bin="${test_root}/bin"
mkdir -p "${fake_bin}" "${test_root}/tmp"

fail() {
  printf 'macOS signing contract test failed: %s\n' "$*" >&2
  exit 1
}

cat >"${fake_bin}/openssl" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  pkcs12)
    output=""
    while [[ $# -gt 0 ]]; do
      if [[ "$1" == "-out" ]]; then
        output="$2"
        break
      fi
      shift
    done
    [[ -n "${output}" ]]
    printf '%s\n' 'fake certificate' >"${output}"
    ;;
  x509)
    if [[ "${CTX_FAKE_WRONG_IDENTITY:-0}" == "1" ]]; then
      printf '%s\n' 'subject=CN=Apple Development: Wrong Identity,OU=BADTEAM'
    elif [[ "${CTX_FAKE_WRONG_TEAM:-0}" == "1" ]]; then
      printf '%s\n' 'subject=CN=Developer ID Application: Other (OTHERTEAM),OU=OTHERTEAM'
    else
      printf '%s\n' 'subject=CN=Developer ID Application: ctx test (TESTTEAM),OU=TESTTEAM'
    fi
    ;;
  pkey) ;;
  *) exit 2 ;;
esac
SH

cat >"${fake_bin}/rcodesign" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
[[ " $* " == *' --for-notarization '* ]]
p12=""
password=""
artifact="${!#}"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --p12-file) p12="$2"; shift 2 ;;
    --p12-password-file) password="$2"; shift 2 ;;
    *) shift ;;
  esac
done
[[ "$(stat -c '%a' "${p12}" 2>/dev/null || stat -f '%Lp' "${p12}")" == "600" ]]
[[ "$(stat -c '%a' "${password}" 2>/dev/null || stat -f '%Lp' "${password}")" == "600" ]]
if [[ "${CTX_FAKE_SIGN_FAILURE:-0}" == "1" ]]; then
  exit 17
fi
printf '%s\n' 'FAKE_DEVELOPER_ID_SIGNATURE' >>"${artifact}"
SH

cat >"${fake_bin}/codesign" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
artifact="${!#}"
grep -Fq 'FAKE_DEVELOPER_ID_SIGNATURE' "${artifact}" || exit 1
if [[ "${1:-}" == "-d" ]]; then
  cat >&2 <<'DETAILS'
Executable=fake
Identifier=rs.ctx.test
Authority=Developer ID Application: ctx test (TESTTEAM)
TeamIdentifier=TESTTEAM
Timestamp=Jul 12, 2026 at 12:00:00 PM
flags=0x10000(runtime) hashes=2+7 location=embedded
DETAILS
fi
SH

cat >"${fake_bin}/ditto" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
source="${@: -2:1}"
output="${@: -1}"
cp "${source}" "${output}"
if [[ "${CTX_FAKE_MUTATE_AFTER_SIGN:-0}" == "1" ]]; then
  printf '%s\n' mutation >>"${source}"
fi
SH

cat >"${fake_bin}/xcrun" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
[[ "${1:-}" == "notarytool" ]]
operation="${2:-}"
case "${operation}" in
  submit)
    [[ " $* " == *' --wait '* ]]
    [[ " $* " == *" --timeout ${CTX_MACOS_NOTARY_TIMEOUT:-30m} "* ]]
    case "${CTX_FAKE_NOTARY_RESULT:-accepted}" in
      accepted)
        printf '%s\n' '{"id":"00000000-0000-0000-0000-000000000001","status":"Accepted"}'
        ;;
      rejected)
        printf '%s\n' '{"id":"00000000-0000-0000-0000-000000000002","status":"Invalid","statusSummary":"rejected"}'
        printf '%s\n' 'notary submission rejected' >&2
        exit 1
        ;;
      timeout)
        printf '%s\n' 'notary submission timed out' >&2
        exit 124
        ;;
      *) exit 2 ;;
    esac
    ;;
  log)
    printf '%s\n' '{"status":"Invalid","issues":[{"severity":"error","path":"ctx","message":"invalid signature"}]}'
    ;;
  *) exit 2 ;;
esac
SH

cat >"${fake_bin}/spctl" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
artifact="${!#}"
grep -Fq 'FAKE_DEVELOPER_ID_SIGNATURE' "${artifact}" || exit 1
printf '%s\n' "${artifact}: accepted source=Notarized Developer ID"
SH
chmod +x "${fake_bin}"/*

export PATH="${fake_bin}:${PATH}"
export TMPDIR="${test_root}/tmp"
export CTX_TEST_ONLY_MACOS_HOST=Darwin
export CTX_MACOS_NOTARY_TIMEOUT=7m
export APPLE_CODESIGN_CERT_PASSWORD='password-secret-sentinel'
export APPLE_CODESIGN_CERT_P12_B64
APPLE_CODESIGN_CERT_P12_B64="$(printf '%s' 'p12-secret-sentinel' | base64 | tr -d '\n')"
export NOTARY_ISSUER='issuer-test-value'
export NOTARY_KEY_ID='key-id-test-value'
export NOTARY_KEY_P8_B64
NOTARY_KEY_P8_B64="$(printf '%s\n' '-----BEGIN PRIVATE KEY-----' 'p8-secret-sentinel' '-----END PRIVATE KEY-----' | base64 | tr -d '\n')"

sign_script="${repo_root}/scripts/sign-notarize-macos-release-artifact.sh"
check_script="${repo_root}/scripts/check-macos-release-signing.sh"
evidence_tool="${repo_root}/scripts/macos-release-signing-evidence.py"

new_artifact() {
  local name="$1"
  local path="${test_root}/${name}"
  printf '%s\n' 'fake thin Mach-O bytes' >"${path}"
  chmod 0755 "${path}"
  printf '%s\n' "${path}"
}

expect_failure() {
  local pattern="$1"
  local log="$2"
  shift 2
  if "$@" >"${log}" 2>&1; then
    fail "command unexpectedly succeeded: $*"
  fi
  grep -Fq "${pattern}" "${log}" || {
    sed -n '1,120p' "${log}" >&2
    fail "failure output did not contain: ${pattern}"
  }
}

success_dir="${test_root}/success"
mkdir -p "${success_dir}"
success_artifact="$(new_artifact success-cli)"
"${sign_script}" macos-arm64 cli "${success_artifact}" "${success_dir}" \
  >"${test_root}/success.log" 2>&1
sha256sum "${success_artifact}" | awk '{print $1}' >"${success_artifact}.sha256"
"${check_script}" macos-arm64 cli "${success_artifact}" \
  "${success_dir}/ctx-macos-arm64.signing.json"
python3 - "${success_artifact}" "${success_dir}/ctx-macos-arm64.signing.json" <<'PY'
import hashlib
import json
import sys

with open(sys.argv[1], "rb") as source:
    digest = hashlib.file_digest(source, "sha256").hexdigest()
with open(sys.argv[2], encoding="utf-8") as source:
    evidence = json.load(source)
assert evidence["artifact_sha256"] == digest
assert evidence["notarization"]["status"] == "Accepted"
assert evidence["codesign"]["hardened_runtime"] is True
assert evidence["codesign"]["secure_timestamp"] is True
assert evidence["gatekeeper"]["verified"] is True
PY
find "${test_root}/tmp" -maxdepth 1 -name 'ctx-macos-signing.*' -print -quit \
  | grep -q . && fail "secret temporary directory was not removed"
for secret in password-secret-sentinel p12-secret-sentinel p8-secret-sentinel; do
  if grep -R -Fq "${secret}" "${success_dir}" "${test_root}/success.log"; then
    fail "secret value appeared in signing output: ${secret}"
  fi
done

for missing in \
  APPLE_CODESIGN_CERT_P12_B64 \
  APPLE_CODESIGN_CERT_PASSWORD \
  NOTARY_ISSUER \
  NOTARY_KEY_ID \
  NOTARY_KEY_P8_B64; do
  artifact="$(new_artifact "missing-${missing}")"
  expect_failure "missing required env var: ${missing}" "${test_root}/missing-${missing}.log" \
    env -u "${missing}" "${sign_script}" macos-arm64 cli "${artifact}" "${test_root}/missing-evidence"
done

artifact="$(new_artifact wrong-identity)"
expect_failure 'not a Developer ID Application identity' "${test_root}/wrong-identity.log" \
  env CTX_FAKE_WRONG_IDENTITY=1 "${sign_script}" macos-arm64 cli "${artifact}" "${test_root}/wrong-identity-evidence"

artifact="$(new_artifact wrong-team)"
expect_failure 'TeamIdentifier does not match the supplied identity' "${test_root}/wrong-team.log" \
  env CTX_FAKE_WRONG_TEAM=1 "${sign_script}" macos-arm64 cli "${artifact}" "${test_root}/wrong-team-evidence"

artifact="$(new_artifact sign-failure)"
expect_failure 'Developer ID signing failed' "${test_root}/sign-failure.log" \
  env CTX_FAKE_SIGN_FAILURE=1 "${sign_script}" macos-arm64 cli "${artifact}" "${test_root}/sign-failure-evidence"

artifact="$(new_artifact rejected)"
expect_failure 'status Invalid' "${test_root}/rejected.log" \
  env CTX_FAKE_NOTARY_RESULT=rejected "${sign_script}" macos-arm64 cli "${artifact}" "${test_root}/rejected-evidence"
[[ -s "${test_root}/rejected-evidence/ctx-macos-arm64.notary-submit.json" ]] || \
  fail "notary rejection JSON was not preserved"
[[ -s "${test_root}/rejected-evidence/ctx-macos-arm64.notary-submit.stderr" ]] || \
  fail "notary rejection stderr was not preserved"
[[ -s "${test_root}/rejected-evidence/ctx-macos-arm64.notary-log.json" ]] || \
  fail "notary rejection log JSON was not preserved"

artifact="$(new_artifact timeout)"
expect_failure 'timed out after 7m' "${test_root}/timeout.log" \
  env CTX_FAKE_NOTARY_RESULT=timeout "${sign_script}" macos-arm64 cli "${artifact}" "${test_root}/timeout-evidence"
[[ -s "${test_root}/timeout-evidence/ctx-macos-arm64.notary-submit.stderr" ]] || \
  fail "notary timeout stderr was not preserved"

artifact="$(new_artifact mutation)"
expect_failure 'mutated after Developer ID signing' "${test_root}/mutation.log" \
  env CTX_FAKE_MUTATE_AFTER_SIGN=1 "${sign_script}" macos-arm64 cli "${artifact}" "${test_root}/mutation-evidence"

ordering_dir="${test_root}/ordering"
mkdir -p "${ordering_dir}"
artifact="$(new_artifact ordering-cli)"
sha256sum "${artifact}" | awk '{print $1}' >"${artifact}.sha256"
"${sign_script}" macos-x64 cli "${artifact}" "${ordering_dir}" >/dev/null
expect_failure 'signed artifact checksum mismatch' "${test_root}/ordering.log" \
  "${check_script}" macos-x64 cli "${artifact}" "${ordering_dir}/ctx-macos-x64.signing.json"
sha256sum "${artifact}" | awk '{print $1}' >"${artifact}.sha256"
"${check_script}" macos-x64 cli "${artifact}" "${ordering_dir}/ctx-macos-x64.signing.json"

runtime_dir="${test_root}/runtime"
mkdir -p "${runtime_dir}/package/lib"
runtime="$(new_artifact runtime-dylib)"
"${sign_script}" macos-x64 runtime "${runtime}" "${runtime_dir}" >/dev/null
cp "${runtime}" "${runtime_dir}/package/lib/libonnxruntime.dylib"
tar -czf "${runtime_dir}/ctx-onnxruntime-macos-x64.tar.gz" \
  -C "${runtime_dir}/package" lib/libonnxruntime.dylib
runtime_archive="${runtime_dir}/ctx-onnxruntime-macos-x64.tar.gz"
sha256sum "${runtime_archive}" | awk '{print $1}' >"${runtime_archive}.sha256"
runtime_evidence="${runtime_dir}/ctx-onnxruntime-macos-x64.signing.json"
python3 "${evidence_tool}" bind-archive \
  --evidence "${runtime_evidence}" \
  --platform macos-x64 \
  --archive "${runtime_archive}" \
  --checksum "${runtime_archive}.sha256" \
  --nested-artifact "${runtime}" \
  --role release
"${check_script}" macos-x64 runtime "${runtime_archive}" "${runtime_evidence}"

printf '%s\n' 'unsigned nested dylib' >"${runtime_dir}/package/lib/libonnxruntime.dylib"
tar -czf "${runtime_archive}" -C "${runtime_dir}/package" lib/libonnxruntime.dylib
sha256sum "${runtime_archive}" | awk '{print $1}' >"${runtime_archive}.sha256"
python3 - "${runtime_evidence}" "${runtime_archive}" "${runtime_dir}/package/lib/libonnxruntime.dylib" <<'PY'
import hashlib
import json
import sys

evidence_path, archive, nested = sys.argv[1:]
with open(evidence_path, encoding="utf-8") as source:
    evidence = json.load(source)
with open(archive, "rb") as source:
    archive_sha = hashlib.file_digest(source, "sha256").hexdigest()
with open(nested, "rb") as source:
    nested_sha = hashlib.file_digest(source, "sha256").hexdigest()
evidence["artifact_sha256"] = nested_sha
evidence["packages"] = [{
    "archive_name": archive.rsplit("/", 1)[-1],
    "archive_sha256": archive_sha,
    "nested_artifact_sha256": nested_sha,
    "role": "release",
}]
with open(evidence_path, "w", encoding="utf-8") as output:
    json.dump(evidence, output, sort_keys=True, separators=(",", ":"))
    output.write("\n")
PY
expect_failure 'strict codesign verification failed' "${test_root}/unsigned-nested.log" \
  "${check_script}" macos-x64 runtime "${runtime_archive}" "${runtime_evidence}"

printf 'macOS release signing contract tests passed\n'
