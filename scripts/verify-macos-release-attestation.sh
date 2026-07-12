#!/usr/bin/env bash
set -euo pipefail

EXPECTED_AUTHORITY="Developer ID Application: Legacy Publisher LLC (SJSNARH4TG)"
EXPECTED_TEAM_ID="SJSNARH4TG"
EXPECTED_CA_SHA256="F1:6C:D3:C5:4C:7F:83:CE:A4:BF:1A:3E:6A:08:19:C8:AA:A8:E4:A1:52:8F:D1:44:71:5F:35:06:43:D2:DF:3A"

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/verify-macos-release-attestation.sh PLATFORM KIND ARTIFACT ATTESTATION CMS

Cryptographically verifies the detached Developer ID CMS attestation and its
binding to one exact macOS CLI or runtime dylib.
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

[[ $# -eq 5 ]] || { usage; exit 2; }
platform="$1"
kind="$2"
artifact="$3"
attestation="$4"
cms="$5"
case "${platform}" in macos-arm64|macos-x64) ;; *) usage; exit 2 ;; esac
case "${kind}" in cli|runtime) ;; *) usage; exit 2 ;; esac
[[ -f "${artifact}" ]] || die "attested macOS artifact missing: ${artifact}"
[[ -s "${attestation}" ]] || die "macOS attestation statement missing: ${attestation}"
[[ -s "${cms}" ]] || die "macOS attestation signature missing: ${cms}"
command -v openssl >/dev/null 2>&1 || die "openssl is required to verify macOS attestation"
command -v python3 >/dev/null 2>&1 || die "python3 is required to verify macOS attestation"

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ca_file="${root_dir}/scripts/apple-developer-id-g2-ca.pem"
ca_fingerprint="$(openssl x509 -in "${ca_file}" -noout -fingerprint -sha256 2>/dev/null \
  | sed 's/^.*Fingerprint=//')"
[[ "${ca_fingerprint}" == "${EXPECTED_CA_SHA256}" ]] || \
  die "pinned Apple Developer ID G2 CA fingerprint mismatch"
work_dir="$(mktemp -d "${TMPDIR:-/tmp}/ctx-macos-attestation-check.XXXXXX")"
trap 'rm -rf "${work_dir}"' EXIT
signer_cert="${work_dir}/signer.pem"
if ! openssl cms -verify \
  -binary \
  -inform DER \
  -in "${cms}" \
  -content "${attestation}" \
  -purpose any \
  -partial_chain \
  -CAfile "${ca_file}" \
  -certsout "${signer_cert}" \
  -out /dev/null >/dev/null 2>&1; then
  die "macOS release attestation CMS signature verification failed"
fi
subject="$(openssl x509 \
  -in "${signer_cert}" -noout -subject -nameopt RFC2253 2>/dev/null || true)"
subject=",${subject#subject=},"
[[ "${subject}" == *",CN=${EXPECTED_AUTHORITY},"* ]] || \
  die "macOS attestation signer does not have the pinned ctx Apple authority"
[[ "${subject}" == *",OU=${EXPECTED_TEAM_ID},"* ]] || \
  die "macOS attestation signer does not have the pinned ctx Apple Team ID"

python3 "${root_dir}/scripts/macos-release-signing-evidence.py" verify-attestation \
  --attestation "${attestation}" \
  --platform "${platform}" \
  --kind "${kind}" \
  --artifact "${artifact}" \
  --source-commit "$(git -C "${root_dir}" rev-parse --verify HEAD)"
printf 'macOS release attestation ok: %s %s\n' "${platform}" "${kind}"
