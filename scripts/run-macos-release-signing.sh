#!/usr/bin/env bash
set -euo pipefail
case "$-" in
  *x*) set +x ;;
esac

INFISICAL_PROJECT_ID="590927ab-758e-41b0-9e15-4cf070e87cf4"
INFISICAL_ENVIRONMENT="prod"
INFISICAL_SECRET_PATH="/"
SIGNING_SECRET_NAMES=(
  APPLE_CODESIGN_CERT_P12_B64
  APPLE_CODESIGN_CERT_PASSWORD
  NOTARY_ISSUER
  NOTARY_KEY_ID
  NOTARY_KEY_P8_B64
)

usage() {
  cat >&2 <<'USAGE'
Usage:
  scripts/run-macos-release-signing.sh --preflight
  scripts/run-macos-release-signing.sh PLATFORM KIND ARTIFACT [EVIDENCE_DIR]

Runs the trusted macOS signing preflight or invokes the signer with a minimal
operational environment and one path to five protected secret files.
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "missing required macOS signing tool: $1"
}

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
"${root_dir}/scripts/check-macos-signing-trusted-ref.sh" >/dev/null
if [[ "${CTX_TEST_ONLY_MACOS_HOST:-}" == "Darwin" ]]; then
  [[ "${CTX_LOCAL_MACOS_SIGNING_LIVE_TEST:-0}" == "1" ]] || \
    die "CTX_TEST_ONLY_MACOS_HOST is restricted to non-CI local contract tests"
elif [[ "$(uname -s)" != "Darwin" ]]; then
  die "macOS release signing requires a native Darwin runner"
fi

for command_name in base64 codesign ditto find git openssl python3 rcodesign spctl stat xcode-select xcrun; do
  require_command "${command_name}"
done
xcode-select -p >/dev/null 2>&1 || die "xcode-select has no active developer directory"
xcrun notarytool --version >/dev/null 2>&1 || die "xcrun notarytool is unavailable"
rcodesign --version >/dev/null 2>&1 || die "rcodesign version check failed"
openssl cms -help >/dev/null 2>&1 || die "OpenSSL CMS support is required"

secret_source="${CTX_MACOS_SIGNING_SECRET_SOURCE:-}"
if [[ -z "${secret_source}" ]]; then
  if [[ "${BUILDKITE:-}" == "true" || "${BUILDKITE:-}" == "1" ]]; then
    secret_source=infisical
  else
    secret_source=injected
  fi
fi
case "${secret_source}" in
  infisical)
    require_command infisical
    infisical --version >/dev/null 2>&1 || die "Infisical CLI version check failed"
    ;;
  injected)
    if [[ "${BUILDKITE:-}" == "true" || "${BUILDKITE:-}" == "1" ]]; then
      die "Buildkite macOS signing must fetch its five values through Infisical"
    fi
    ;;
  *) die "CTX_MACOS_SIGNING_SECRET_SOURCE must be infisical or injected" ;;
esac

umask 077
secret_root="$(mktemp -d "${TMPDIR:-/tmp}/ctx-macos-signing-launcher.XXXXXX")"
chmod 0700 "${secret_root}"
cleanup() {
  rm -rf "${secret_root}" >/dev/null 2>&1 || true
}
trap cleanup EXIT

fetch_secret() {
  local name="$1"
  local output="${secret_root}/${name}"
  local diagnostic="${secret_root}/${name}.stderr"

  case "${secret_source}" in
    infisical)
      if ! infisical secrets get "${name}" \
        --plain \
        --projectId "${INFISICAL_PROJECT_ID}" \
        --env "${INFISICAL_ENVIRONMENT}" \
        --path "${INFISICAL_SECRET_PATH}" \
        >"${output}" 2>"${diagnostic}"; then
        die "Infisical lookup failed for required macOS signing value ${name}"
      fi
      rm -f "${diagnostic}"
      ;;
    injected)
      [[ -n "${!name:-}" ]] || die "missing required injected macOS signing value ${name}"
      printf '%s' "${!name}" >"${output}"
      ;;
  esac
  chmod 0600 "${output}"
  [[ -s "${output}" ]] || die "required macOS signing value ${name} was empty"
}

for secret_name in "${SIGNING_SECRET_NAMES[@]}"; do
  fetch_secret "${secret_name}"
done

if [[ "${1:-}" == "--preflight" ]]; then
  [[ $# -eq 1 ]] || { usage; exit 2; }
  printf 'macOS signing preflight ok: tools, trusted ref, Infisical auth, and 5 allowlisted values\n'
  exit 0
fi

platform="${1:-}"
kind="${2:-}"
artifact="${3:-}"
evidence_dir="${4:-target/public-cli-artifacts}"
if [[ -z "${platform}" || -z "${kind}" || -z "${artifact}" || $# -gt 4 ]]; then
  usage
  exit 2
fi

signer_path="${root_dir}/scripts/sign-notarize-macos-release-artifact.sh"
if [[ -n "${CTX_TEST_ONLY_MACOS_SIGNER_PATH:-}" ]]; then
  [[ "${CTX_LOCAL_MACOS_SIGNING_LIVE_TEST:-0}" == "1" \
    && "${CTX_TEST_ONLY_MACOS_HOST:-}" == "Darwin" \
    && "${CTX_TEST_ONLY_MACOS_SIGNER_PATH}" == /* \
    && -x "${CTX_TEST_ONLY_MACOS_SIGNER_PATH}" ]] || \
    die "CTX_TEST_ONLY_MACOS_SIGNER_PATH is restricted to non-CI local contract tests"
  signer_path="${CTX_TEST_ONLY_MACOS_SIGNER_PATH}"
fi

minimal_env=(
  "PATH=${PATH}"
  "HOME=${HOME:-/var/empty}"
  "TMPDIR=${TMPDIR:-/tmp}"
  "LANG=${LANG:-C}"
  "LC_ALL=${LC_ALL:-C}"
  "CTX_MACOS_SIGNING_LAUNCHED=1"
  "CTX_MACOS_SIGNING_SECRET_DIR=${secret_root}"
  "CTX_MACOS_NOTARY_TIMEOUT=${CTX_MACOS_NOTARY_TIMEOUT:-30m}"
)
for operational_name in \
  BUILDKITE BUILDKITE_BRANCH BUILDKITE_COMMIT BUILDKITE_PULL_REQUEST \
  BUILDKITE_REPO BUILDKITE_TAG CTX_LOCAL_MACOS_SIGNING_LIVE_TEST \
  CTX_TEST_ONLY_MACOS_HOST DEVELOPER_DIR LOGNAME USER; do
  if [[ -n "${!operational_name:-}" ]]; then
    minimal_env+=("${operational_name}=${!operational_name}")
  fi
done

env -i "${minimal_env[@]}" \
  "${signer_path}" \
  "${platform}" "${kind}" "${artifact}" "${evidence_dir}"
