#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/stage-github-release-assets.sh [ARTIFACT_DIR] [OUT_DIR]

Stages public GitHub Release assets from built public CLI artifacts.

Inputs default to target/public-cli-artifacts.
Outputs default to target/github-release-assets.
USAGE
}

artifact_dir="${1:-target/public-cli-artifacts}"
out_dir="${2:-target/github-release-assets}"

if [[ "${artifact_dir}" == "-h" || "${artifact_dir}" == "--help" ]]; then
  usage
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

sha256_file() {
  local path="$1"

  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "${path}" | awk '{ print $1 }'
    return
  fi

  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "${path}" | awk '{ print $1 }'
    return
  fi

  printf 'sha256sum or shasum is required\n' >&2
  exit 127
}

stage_asset() {
  local source_name="$1"
  local dest_name="$2"
  local source_path="${artifact_dir%/}/${source_name}"
  local dest_path="${out_dir%/}/${dest_name}"

  if [[ ! -f "${source_path}" ]]; then
    printf 'missing public CLI artifact: %s\n' "${source_path}" >&2
    exit 1
  fi

  install -m 0755 "${source_path}" "${dest_path}"
  printf '%s  %s\n' "$(sha256_file "${dest_path}")" "${dest_name}" >> "${out_dir%/}/SHA256SUMS"
}

mkdir -p "${out_dir}"
rm -f \
  "${out_dir%/}/ctx-linux-x64" \
  "${out_dir%/}/ctx-macos-arm64" \
  "${out_dir%/}/ctx-macos-x64" \
  "${out_dir%/}/ctx-windows-x64.exe" \
  "${out_dir%/}/ctx-freebsd-x64" \
  "${out_dir%/}/SHA256SUMS"

stage_asset ctx ctx-linux-x64
stage_asset ctx-macos-arm64 ctx-macos-arm64
stage_asset ctx-macos-x64 ctx-macos-x64
stage_asset ctx.exe ctx-windows-x64.exe
stage_asset ctx-freebsd-x64 ctx-freebsd-x64

printf 'staged GitHub release assets in %s\n' "${out_dir}"
