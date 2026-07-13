#!/usr/bin/env bash
set -euo pipefail

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

[[ $# -eq 4 ]] || {
  printf 'usage: %s PLATFORM CLI EXPECTED_VERSION EVIDENCE\n' "$0" >&2
  exit 2
}
platform="$1"
artifact="$2"
expected_version="$3"
evidence="$4"
case "${platform}" in macos-arm64|macos-x64) ;; *) die "unsupported macOS platform" ;; esac
[[ -f "${artifact}" ]] || die "signed macOS CLI missing: ${artifact}"
[[ -s "${evidence}" ]] || die "macOS signing evidence missing: ${evidence}"
if [[ "${CTX_TEST_ONLY_MACOS_HOST:-}" == "Darwin" ]]; then
  [[ "${CTX_LOCAL_MACOS_SIGNING_LIVE_TEST:-0}" == "1" ]] || \
    die "CTX_TEST_ONLY_MACOS_HOST is restricted to local contract tests"
elif [[ "$(uname -s)" != "Darwin" ]]; then
  die "signed macOS CLI verification requires a native Darwin host"
fi
command -v python3 >/dev/null 2>&1 || die "python3 is required"

timeout_seconds="${CTX_MACOS_CLI_EXEC_TIMEOUT_SECONDS:-30}"
[[ "${timeout_seconds}" =~ ^[1-9][0-9]*$ ]] || \
  die "CTX_MACOS_CLI_EXEC_TIMEOUT_SECONDS must be a positive integer"
artifact="$(cd "$(dirname "${artifact}")" && pwd)/$(basename "${artifact}")"
artifact_sha="$(sha256_file "${artifact}")"
umask 077
work_dir="$(mktemp -d "${TMPDIR:-/tmp}/ctx-macos-signed-cli-check.XXXXXX")"
trap 'rm -rf "${work_dir}"' EXIT
candidate="${work_dir}/ctx"
output="${work_dir}/version.txt"
cp "${artifact}" "${candidate}"
chmod 0755 "${candidate}"
[[ "$(sha256_file "${candidate}")" == "${artifact_sha}" ]] || \
  die "temporary execution candidate does not match signed CLI bytes"

python3 - "${candidate}" "ctx ${expected_version}" "${timeout_seconds}" "${output}" <<'PY'
import os
import subprocess
import sys
from pathlib import Path

candidate, expected, timeout, output = sys.argv[1:]
environment = {
    "HOME": str(Path(candidate).parent),
    "LANG": "C",
    "LC_ALL": "C",
    "PATH": "/usr/bin:/bin",
    "TMPDIR": str(Path(candidate).parent),
}
try:
    result = subprocess.run(
        [candidate, "--version"],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        timeout=int(timeout),
        env=environment,
        check=False,
    )
except subprocess.TimeoutExpired as error:
    raise SystemExit(f"signed CLI --version timed out after {timeout}s") from error
if result.returncode != 0:
    raise SystemExit(f"signed CLI --version exited with status {result.returncode}")
version = result.stdout.strip()
if version != expected:
    raise SystemExit(f"signed CLI returned unexpected version: {version!r}")
Path(output).write_text(version + "\n", encoding="utf-8")
PY

[[ "$(sha256_file "${candidate}")" == "${artifact_sha}" \
  && "$(sha256_file "${artifact}")" == "${artifact_sha}" ]] || \
  die "CLI bytes mutated during signed execution verification"
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
python3 "${root_dir}/scripts/macos-release-signing-evidence.py" \
  record-cli-execution-verification \
  --evidence "${evidence}" \
  --platform "${platform}" \
  --artifact "${artifact}" \
  --version-output "$(tr -d '\r\n' <"${output}")"
diagnostic="$(dirname "${evidence}")/ctx-${platform}.execution.txt"
{
  printf 'method=signed-exact-byte-version-execution\n'
  printf 'status=passed\n'
  printf 'artifact_sha256=%s\n' "${artifact_sha}"
  printf 'version_output=%s\n' "$(tr -d '\r\n' <"${output}")"
} >"${diagnostic}"
chmod 0644 "${diagnostic}"
printf 'signed macOS CLI verification passed: %s\n' "${artifact}"
