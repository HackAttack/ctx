#!/usr/bin/env bash
set -euo pipefail
case "$-" in
  *x*) set +x ;;
esac

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/sign-notarize-macos-release-artifact.sh PLATFORM KIND ARTIFACT [EVIDENCE_DIR]

Signs one standalone macOS release Mach-O with Developer ID, submits a
temporary ZIP to Apple notarization, and records sanitized verification
evidence. KIND is cli or runtime.
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

require_env() {
  local name="$1"
  [[ -n "${!name:-}" ]] || die "missing required env var: ${name}"
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

decode_b64_file() {
  local env_name="$1"
  local output="$2"
  local value="${!env_name}"

  rm -f "${output}"
  if printf '%s' "${value}" | base64 --decode >"${output}" 2>/dev/null \
    || printf '%s' "${value}" | base64 -d >"${output}" 2>/dev/null \
    || printf '%s' "${value}" | base64 -D >"${output}" 2>/dev/null; then
    chmod 0600 "${output}"
    [[ -s "${output}" ]] || die "decoded ${env_name} was empty"
    return 0
  fi
  rm -f "${output}"
  die "failed to decode ${env_name}"
}

extract_codesign_certificate() {
  local p12_path="$1"
  local password_path="$2"
  local certificate_path="$3"

  rm -f "${certificate_path}"
  if openssl pkcs12 \
    -in "${p12_path}" -passin "file:${password_path}" \
    -clcerts -nokeys -out "${certificate_path}" >/dev/null 2>&1; then
    chmod 0600 "${certificate_path}"
    return 0
  fi
  rm -f "${certificate_path}"
  if openssl pkcs12 -legacy \
    -in "${p12_path}" -passin "file:${password_path}" \
    -clcerts -nokeys -out "${certificate_path}" >/dev/null 2>&1; then
    chmod 0600 "${certificate_path}"
    return 0
  fi
  die "APPLE_CODESIGN_CERT_P12_B64 could not be opened with APPLE_CODESIGN_CERT_PASSWORD"
}

json_field() {
  local path="$1"
  local name="$2"
  python3 - "${path}" "${name}" <<'PY'
import json
import sys

try:
    with open(sys.argv[1], encoding="utf-8") as source:
        value = json.load(source).get(sys.argv[2])
except (OSError, json.JSONDecodeError, AttributeError):
    value = None
if value is not None:
    print(value, end="")
PY
}

sha256_file() {
  local path="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "${path}" | awk '{ print $1 }'
  else
    shasum -a 256 "${path}" | awk '{ print $1 }'
  fi
}

print_notary_diagnostics() {
  local submit_stderr="$1"
  local log_json="$2"
  local log_stderr="$3"

  if [[ -s "${submit_stderr}" ]]; then
    sed -n '1,40p' "${submit_stderr}" >&2 || true
  fi
  if [[ -s "${log_json}" ]]; then
    python3 - "${log_json}" <<'PY' >&2 || true
import json
import sys

try:
    with open(sys.argv[1], encoding="utf-8") as source:
        payload = json.load(source)
except (OSError, json.JSONDecodeError):
    raise SystemExit(0)
issues = payload.get("issues") if isinstance(payload, dict) else None
if isinstance(issues, list):
    for issue in issues[:20]:
        if isinstance(issue, dict):
            print(": ".join(str(issue[key]) for key in ("severity", "path", "message") if issue.get(key)))
PY
  elif [[ -s "${log_stderr}" ]]; then
    sed -n '1,40p' "${log_stderr}" >&2 || true
  fi
}

platform="${1:-}"
kind="${2:-}"
artifact="${3:-}"
evidence_dir="${4:-target/public-cli-artifacts}"
if [[ -z "${platform}" || -z "${kind}" || -z "${artifact}" ]]; then
  usage
  exit 2
fi
case "${platform}" in
  macos-arm64|macos-x64) ;;
  *) usage; exit 2 ;;
esac
case "${kind}" in
  cli)
    evidence_prefix="ctx-${platform}"
    ;;
  runtime)
    evidence_prefix="ctx-onnxruntime-${platform}"
    ;;
  *) usage; exit 2 ;;
esac
[[ -f "${artifact}" ]] || die "macOS release artifact not found: ${artifact}"
if [[ "${CTX_TEST_ONLY_MACOS_HOST:-}" != "Darwin" && "$(uname -s)" != "Darwin" ]]; then
  die "macOS release signing requires a native Darwin host"
fi

for name in \
  APPLE_CODESIGN_CERT_P12_B64 \
  APPLE_CODESIGN_CERT_PASSWORD \
  NOTARY_ISSUER \
  NOTARY_KEY_ID \
  NOTARY_KEY_P8_B64; do
  require_env "${name}"
done
for command_name in base64 codesign ditto openssl python3 rcodesign spctl xcrun; do
  require_command "${command_name}"
done

notary_timeout="${CTX_MACOS_NOTARY_TIMEOUT:-30m}"
[[ "${notary_timeout}" =~ ^[1-9][0-9]*[smh]$ ]] || \
  die "CTX_MACOS_NOTARY_TIMEOUT must be a positive integer followed by s, m, or h"

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
mkdir -p "${evidence_dir}"
evidence_dir="$(cd "${evidence_dir}" && pwd)"
artifact="$(cd "$(dirname "${artifact}")" && pwd)/$(basename "${artifact}")"
submit_json="${evidence_dir}/${evidence_prefix}.notary-submit.json"
submit_stderr="${evidence_dir}/${evidence_prefix}.notary-submit.stderr"
log_json="${evidence_dir}/${evidence_prefix}.notary-log.json"
log_stderr="${evidence_dir}/${evidence_prefix}.notary-log.stderr"
codesign_details="${evidence_dir}/${evidence_prefix}.codesign.txt"
gatekeeper_details="${evidence_dir}/${evidence_prefix}.gatekeeper.txt"
evidence_json="${evidence_dir}/${evidence_prefix}.signing.json"
rm -f "${submit_json}" "${submit_stderr}" "${log_json}" "${log_stderr}" \
  "${codesign_details}" "${gatekeeper_details}" "${evidence_json}"

umask 077
secret_root="$(mktemp -d "${TMPDIR:-/tmp}/ctx-macos-signing.XXXXXX")"
cleanup() {
  rm -rf "${secret_root}" >/dev/null 2>&1 || true
}
trap cleanup EXIT
cert_path="${secret_root}/codesign-cert.p12"
cert_password_path="${secret_root}/codesign-cert.password"
cert_pem_path="${secret_root}/codesign-cert.pem"
notary_key_path="${secret_root}/AuthKey.p8"
notary_zip="${secret_root}/${evidence_prefix}.zip"

decode_b64_file APPLE_CODESIGN_CERT_P12_B64 "${cert_path}"
printf '%s' "${APPLE_CODESIGN_CERT_PASSWORD}" | tr -d '\r' >"${cert_password_path}"
chmod 0600 "${cert_password_path}"
[[ -s "${cert_password_path}" ]] || die "APPLE_CODESIGN_CERT_PASSWORD was empty"
extract_codesign_certificate "${cert_path}" "${cert_password_path}" "${cert_pem_path}"
certificate_subject="$(openssl x509 \
  -in "${cert_pem_path}" -noout -subject -nameopt RFC2253 2>/dev/null || true)"
[[ "${certificate_subject}" == *"CN=Developer ID Application:"* ]] || \
  die "APPLE_CODESIGN_CERT_P12_B64 is not a Developer ID Application identity"
certificate_team_id="$(printf '%s\n' "${certificate_subject}" \
  | sed -n 's/^subject=.*OU=\([^,]*\).*$/\1/p')"
[[ -n "${certificate_team_id}" ]] || \
  die "Developer ID Application certificate is missing its Team ID"

decode_b64_file NOTARY_KEY_P8_B64 "${notary_key_path}"
grep -Fq 'BEGIN PRIVATE KEY' "${notary_key_path}" || \
  die "NOTARY_KEY_P8_B64 did not decode to a PKCS#8 private key"
openssl pkey -in "${notary_key_path}" -noout >/dev/null 2>&1 || \
  die "NOTARY_KEY_P8_B64 did not decode to a valid private key"

if ! rcodesign sign \
  --for-notarization \
  --p12-file "${cert_path}" \
  --p12-password-file "${cert_password_path}" \
  "${artifact}"; then
  die "Developer ID signing failed for ${platform} ${kind}"
fi
codesign --verify --strict --verbose=4 "${artifact}" >/dev/null 2>&1 || \
  die "strict codesign verification failed for ${platform} ${kind}"
codesign -d --verbose=4 "${artifact}" >"${codesign_details}" 2>&1 || \
  die "could not inspect Developer ID signature for ${platform} ${kind}"
chmod 0644 "${codesign_details}"
grep -Eq '^Authority=Developer ID Application:' "${codesign_details}" || \
  die "signed ${platform} ${kind} has the wrong Developer ID authority"
grep -Fqx "TeamIdentifier=${certificate_team_id}" "${codesign_details}" || \
  die "signed ${platform} ${kind} TeamIdentifier does not match the supplied identity"
grep -Eiq '^flags=.*runtime' "${codesign_details}" || \
  die "signed ${platform} ${kind} is missing hardened runtime flags"
grep -Eq '^Timestamp=.+$' "${codesign_details}" || \
  die "signed ${platform} ${kind} is missing a secure timestamp"
signed_sha256="$(sha256_file "${artifact}")"

ditto -c -k --keepParent "${artifact}" "${notary_zip}" || \
  die "failed to create temporary notarization ZIP for ${platform} ${kind}"
set +e
xcrun notarytool submit "${notary_zip}" \
  --key "${notary_key_path}" \
  --key-id "${NOTARY_KEY_ID}" \
  --issuer "${NOTARY_ISSUER}" \
  --wait \
  --timeout "${notary_timeout}" \
  --output-format json >"${submit_json}" 2>"${submit_stderr}"
submit_status=$?
set -e
chmod 0644 "${submit_json}" "${submit_stderr}" 2>/dev/null || true
notary_status="$(json_field "${submit_json}" status || true)"
submission_id="$(json_field "${submit_json}" id || true)"
if [[ "${submit_status}" -ne 0 || "${notary_status}" != "Accepted" ]]; then
  if [[ -n "${submission_id}" ]]; then
    xcrun notarytool log "${submission_id}" \
      --key "${notary_key_path}" \
      --key-id "${NOTARY_KEY_ID}" \
      --issuer "${NOTARY_ISSUER}" \
      --output-format json >"${log_json}" 2>"${log_stderr}" || true
    chmod 0644 "${log_json}" "${log_stderr}" 2>/dev/null || true
  fi
  print_notary_diagnostics "${submit_stderr}" "${log_json}" "${log_stderr}"
  if [[ "${submit_status}" -eq 124 ]]; then
    die "Apple notarization timed out after ${notary_timeout} for ${platform} ${kind}"
  fi
  die "Apple notarization failed for ${platform} ${kind} with status ${notary_status:-unknown}"
fi

codesign --verify --strict --verbose=4 "${artifact}" >/dev/null 2>&1 || \
  die "post-notarization codesign verification failed for ${platform} ${kind}"
if ! spctl --assess --type execute --verbose=4 "${artifact}" >"${gatekeeper_details}" 2>&1; then
  chmod 0644 "${gatekeeper_details}" 2>/dev/null || true
  sed -n '1,40p' "${gatekeeper_details}" >&2 || true
  die "Gatekeeper rejected notarized ${platform} ${kind}"
fi
chmod 0644 "${gatekeeper_details}"
grep -Fq 'Notarized Developer ID' "${gatekeeper_details}" || \
  die "Gatekeeper did not report Notarized Developer ID for ${platform} ${kind}"
final_sha256="$(sha256_file "${artifact}")"
[[ "${final_sha256}" == "${signed_sha256}" ]] || \
  die "${platform} ${kind} mutated after Developer ID signing"

python3 "${root_dir}/scripts/macos-release-signing-evidence.py" write \
  --output "${evidence_json}" \
  --platform "${platform}" \
  --kind "${kind}" \
  --artifact "${artifact}" \
  --codesign-details "${codesign_details}" \
  --notary-submit "${submit_json}" \
  --gatekeeper-details "${gatekeeper_details}"
printf 'signed and notarized %s %s sha256=%s evidence=%s\n' \
  "${platform}" "${kind}" "${final_sha256}" "${evidence_json}"
