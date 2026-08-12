#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/validate-public-cli-factory-artifact.sh PLATFORM ARTIFACT_DIR OUTPUT_DIR

Validates one exact artifact downloaded from the Linux release factory on its
native platform. This command never compiles or replaces the candidate bytes.
USAGE
}

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

[[ $# -eq 3 ]] || { usage; exit 2; }
platform="$1"
artifact_dir="$2"
output_dir="$3"
case "${platform}" in
  linux-x64) binary="ctx" ;;
  linux-aarch64) binary="ctx-linux-aarch64" ;;
  macos-arm64) binary="ctx-macos-arm64" ;;
  macos-x64) binary="ctx-macos-x64" ;;
  windows-x64) binary="ctx.exe" ;;
  *) usage; exit 2 ;;
esac

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"
artifact="${artifact_dir%/}/${binary}"
[[ -f "${artifact}" && ! -L "${artifact}" ]] || die "factory artifact is missing"
[[ -s "${artifact}.sha256" ]] || die "factory checksum is missing"
before="$(sha256_file "${artifact}")"
[[ "${before}" == "$(tr -d '[:space:]' <"${artifact}.sha256")" ]] || \
  die "factory artifact checksum mismatch"
version="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; d=json.load(sys.stdin); print(next(p["version"] for p in d["packages"] if p["name"]=="ctx"))')"

case "${platform}" in
  macos-arm64|macos-x64)
    scripts/verify-macos-signed-cli.sh "${platform}" "${artifact}" "${version}" \
      "${artifact_dir%/}/ctx-${platform}.signing.json"
    scripts/check-macos-release-signing.sh "${platform}" cli "${artifact}"
    ;;
  windows-x64)
    command -v powershell.exe >/dev/null 2>&1 || die "PowerShell is required"
    printf '%s\n' "${version}" >"${artifact}.expected-version"
    mkdir -p "${output_dir}"
    powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass \
      -File scripts/run-native-candidate-smoke.ps1 \
      -Binary "${artifact}" \
      -Fixture tests/fixtures/custom-history-jsonl/basic.jsonl \
      -ExpectedVersion "${version}" \
      -ResultPath "${output_dir%/}/candidate-smoke.json"
    ;;
  *)
    mkdir -p "${output_dir}"
    scripts/run-native-candidate-smoke.sh \
      "${artifact}" tests/fixtures/custom-history-jsonl/basic.jsonl "${version}" \
      "${output_dir%/}/candidate-smoke.json"
    ;;
esac

after="$(sha256_file "${artifact}")"
[[ "${after}" == "${before}" ]] || die "native validation mutated candidate bytes"
printf 'native exact-byte validation passed: %s sha256=%s\n' "${platform}" "${after}"
