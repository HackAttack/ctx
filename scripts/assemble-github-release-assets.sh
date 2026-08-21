#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/assemble-github-release-assets.sh CORE_DIR RUNTIME_DIR [OUT_DIR]

Combines an independently staged Core GitHub handoff with the five qualified
ONNX Runtime transports. OUT_DIR defaults to target/github-release-assets.
The input directories are never modified and the output is published once.
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

[[ $# -ge 2 && $# -le 3 ]] || {
  usage
  exit 2
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bundle_tool="${repo_root}/scripts/release/release_bundle.py"
core_dir="$1"
runtime_dir="$2"
out_dir="${3:-target/github-release-assets}"
for variable in core_dir runtime_dir out_dir; do
  value="${!variable}"
  [[ "${value}" != -* ]] || die "release directory cannot start with '-': ${value}"
  if [[ "${value}" != /* ]]; then
    printf -v "${variable}" '%s/%s' "${repo_root}" "${value}"
  fi
done

python3 -I "${bundle_tool}" require-directory --directory "${core_dir}"
python3 -I "${bundle_tool}" require-directory --directory "${runtime_dir}"
python3 -I "${bundle_tool}" preflight-publication \
  --input-dir "${core_dir}" --output-dir "${out_dir}"
python3 -I "${bundle_tool}" preflight-publication \
  --input-dir "${runtime_dir}" --output-dir "${out_dir}"

core_assets=(
  ctx-linux-x64
  ctx-linux-x64.cdx.json
  ctx-linux-x64.third-party-notices.txt
  ctx-linux-aarch64
  ctx-linux-aarch64.cdx.json
  ctx-linux-aarch64.third-party-notices.txt
  ctx-macos-arm64
  ctx-macos-arm64.cdx.json
  ctx-macos-arm64.third-party-notices.txt
  ctx-macos-x64
  ctx-macos-x64.cdx.json
  ctx-macos-x64.third-party-notices.txt
  ctx-windows-x64.exe
  ctx-windows-x64.exe.cdx.json
  ctx-windows-x64.exe.third-party-notices.txt
)
runtime_assets=(
  ctx-onnxruntime-linux-x64.tar.gz
  ctx-onnxruntime-linux-aarch64.tar.gz
  ctx-onnxruntime-macos-arm64.tar.gz
  ctx-onnxruntime-macos-x64.tar.gz
  ctx-onnxruntime-windows-x64.zip
)
release_assets=(
  ctx-linux-aarch64
  ctx-linux-aarch64.cdx.json
  ctx-linux-aarch64.third-party-notices.txt
  ctx-linux-x64
  ctx-linux-x64.cdx.json
  ctx-linux-x64.third-party-notices.txt
  ctx-macos-arm64
  ctx-macos-arm64.cdx.json
  ctx-macos-arm64.third-party-notices.txt
  ctx-macos-x64
  ctx-macos-x64.cdx.json
  ctx-macos-x64.third-party-notices.txt
  ctx-onnxruntime-linux-aarch64.tar.gz
  ctx-onnxruntime-linux-x64.tar.gz
  ctx-onnxruntime-macos-arm64.tar.gz
  ctx-onnxruntime-macos-x64.tar.gz
  ctx-onnxruntime-windows-x64.zip
  ctx-windows-x64.exe
  ctx-windows-x64.exe.cdx.json
  ctx-windows-x64.exe.third-party-notices.txt
)

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{ print $1 }'
  else
    die "sha256sum or shasum is required"
  fi
}

require_regular() {
  [[ -f "$1" && ! -L "$1" ]] || die "$2 must be a regular non-symlink file: $1"
}

declare -A core_digests=()
core_names=()
core_sums="${core_dir%/}/SHA256SUMS"
require_regular "${core_sums}" "Core checksum manifest"
while IFS= read -r line; do
  [[ "${line}" =~ ^([0-9a-f]{64})\ \ ([A-Za-z0-9][A-Za-z0-9._-]{0,127})$ ]] \
    || die "Core SHA256SUMS is malformed"
  digest="${BASH_REMATCH[1]}"
  name="${BASH_REMATCH[2]}"
  [[ ! -v "core_digests[${name}]" ]] || die "Core SHA256SUMS repeats ${name}"
  core_digests["${name}"]="${digest}"
  core_names+=("${name}")
done < "${core_sums}"
[[ "${#core_names[@]}" -eq "${#core_assets[@]}" ]] \
  || die "Core SHA256SUMS must contain exactly 15 assets"
for asset in "${core_assets[@]}"; do
  source_path="${core_dir%/}/${asset}"
  require_regular "${source_path}" "Core release asset"
  [[ -v "core_digests[${asset}]" ]] || die "Core SHA256SUMS is missing ${asset}"
  actual="$(sha256_file "${source_path}")"
  [[ "${actual}" == "${core_digests[${asset}]}" ]] \
    || die "Core checksum mismatch for ${asset}"
done

declare -A runtime_digests=()
for asset in "${runtime_assets[@]}"; do
  source_path="${runtime_dir%/}/${asset}"
  checksum_path="${source_path}.sha256"
  require_regular "${source_path}" "runtime release asset"
  require_regular "${checksum_path}" "runtime checksum"
  IFS= read -r expected < "${checksum_path}" || true
  [[ "${expected}" =~ ^[0-9a-f]{64}$ ]] \
    || die "runtime checksum is malformed for ${asset}"
  [[ "$(wc -l < "${checksum_path}" | tr -d '[:space:]')" == "1" ]] \
    || die "runtime checksum must contain one line for ${asset}"
  actual="$(sha256_file "${source_path}")"
  [[ "${actual}" == "${expected}" ]] || die "runtime checksum mismatch for ${asset}"
  runtime_digests["${asset}"]="${actual}"
done

staged="$(mktemp -d "$(dirname "${out_dir}")/.github-release-final.XXXXXX")"
cleanup() {
  if [[ -n "${staged:-}" && -d "${staged}" && ! -L "${staged}" ]]; then
    rm -rf -- "${staged}"
  fi
}
trap cleanup EXIT

for asset in "${core_assets[@]}"; do
  mode=0644
  case "${asset}" in
    ctx-linux-x64|ctx-linux-aarch64|ctx-macos-arm64|ctx-macos-x64)
      mode=0755
      ;;
  esac
  install -m "${mode}" "${core_dir%/}/${asset}" "${staged}/${asset}"
  [[ "$(sha256_file "${staged}/${asset}")" == "${core_digests[${asset}]}" ]] \
    || die "Core asset changed while staged: ${asset}"
done
for asset in "${runtime_assets[@]}"; do
  install -m 0644 "${runtime_dir%/}/${asset}" "${staged}/${asset}"
  [[ "$(sha256_file "${staged}/${asset}")" == "${runtime_digests[${asset}]}" ]] \
    || die "runtime asset changed while staged: ${asset}"
done

for asset in "${release_assets[@]}"; do
  printf '%s  %s\n' "$(sha256_file "${staged}/${asset}")" "${asset}" \
    >> "${staged}/SHA256SUMS"
done
[[ "$(find "${staged}" -maxdepth 1 -type f | wc -l)" == "21" ]] \
  || die "final GitHub release inventory is not exactly 21 files"

python3 -I "${bundle_tool}" commit-directory \
  --stage-dir "${staged}" --output-dir "${out_dir}"
trap - EXIT
printf 'assembled GitHub release assets in %s\n' "${out_dir}"
