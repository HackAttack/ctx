#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/check-github-release-assets.sh --macos-llvm-task-root PATH TAG [REPO]

Checks that a published GitHub Release has the complete expected ctx assets and
that SHA256SUMS verifies them. REPO defaults to ctxrs/ctx. The macOS x64 asset
is inspected only with the approved pinned LLVM task authority.
USAGE
}

usage_error() {
  printf 'error: %s\n' "$1" >&2
  usage
  exit 2
}

tag=""
repo="ctxrs/ctx"
seen_repo=0
macos_llvm_task_root=""
seen_macos_llvm_task_root=0
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

while [[ $# -gt 0 ]]; do
  option="$1"
  case "${option}" in
    --macos-llvm-task-root)
      seen_macos_llvm_task_root=$((seen_macos_llvm_task_root + 1))
      [[ "${seen_macos_llvm_task_root}" == "1" ]] \
        || usage_error "duplicate argument: ${option}"
      shift
      [[ $# -gt 0 && -n "$1" ]] || usage_error "${option} requires a value"
      macos_llvm_task_root="$1"
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    -*)
      usage_error "unknown argument: ${option}"
      ;;
    *)
      if [[ -z "${tag}" ]]; then
        tag="${option}"
      elif [[ "${seen_repo}" == "0" ]]; then
        repo="${option}"
        seen_repo=1
      else
        usage_error "unexpected positional argument: ${option}"
      fi
      ;;
  esac
  shift
done

[[ -n "${tag}" ]] || usage_error "release tag is required"
[[ "${seen_macos_llvm_task_root}" == "1" ]] \
  || usage_error "published macos-x64 validation requires --macos-llvm-task-root"

if ! command -v gh >/dev/null 2>&1; then
  printf 'gh is required\n' >&2
  exit 127
fi

sha256_check() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -c -
    return
  fi

  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 -c -
    return
  fi

  printf 'sha256sum or shasum is required\n' >&2
  exit 127
}

expected_assets=(
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
  SHA256SUMS
)

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/ctx-github-release-assets.XXXXXX")"
# This directory is newly created and owner-private, so use its physical
# spelling for the immutable snapshot while caller task roots stay fail-closed.
tmp_dir="$(cd "${tmp_dir}" && pwd -P)"
tmp_parent="$(dirname "${tmp_dir}")"
macos_llvm_snapshot="${tmp_dir}/.macos-llvm-authority"
cleanup() {
  if [[ "${macos_llvm_snapshot:-}" == "${tmp_dir:-}/.macos-llvm-authority" \
    && -d "${macos_llvm_snapshot}" && ! -L "${macos_llvm_snapshot}" ]]; then
    chmod -R u+w "${macos_llvm_snapshot}" 2>/dev/null || true
  fi
  if [[ -n "${tmp_dir:-}" && -n "${tmp_parent:-}" \
    && "${tmp_dir}" == "${tmp_parent}/ctx-github-release-assets."* \
    && "$(dirname "${tmp_dir}")" == "${tmp_parent}" \
    && -d "${tmp_dir}" && ! -L "${tmp_dir}" ]]; then
    rm -rf -- "${tmp_dir}"
  fi
}
trap cleanup EXIT

python3 -B "${repo_root}/scripts/release/macos_llvm_authority.py" snapshot \
  --task-root "${macos_llvm_task_root}" \
  --snapshot-root "${macos_llvm_snapshot}" \
  || {
    echo "approved macOS LLVM task authority could not be snapshotted" >&2
    exit 1
  }
macos_llvm_readobj="${macos_llvm_snapshot}/bin/llvm-readobj"
macos_llvm_objdump="${macos_llvm_snapshot}/bin/llvm-objdump"

expected_file="${tmp_dir}/expected.txt"
actual_file="${tmp_dir}/actual.txt"

printf '%s\n' "${expected_assets[@]}" | sort > "${expected_file}"
gh release view "${tag}" --repo "${repo}" --json assets --jq '.assets[].name' | sort > "${actual_file}"

if ! cmp -s "${expected_file}" "${actual_file}"; then
  printf 'GitHub release assets for %s do not match expected set\n' "${tag}" >&2
  printf '\nExpected:\n' >&2
  cat "${expected_file}" >&2
  printf '\nActual:\n' >&2
  cat "${actual_file}" >&2
  exit 1
fi

for asset in "${expected_assets[@]}"; do
  gh release download "${tag}" --repo "${repo}" --dir "${tmp_dir}" --pattern "${asset}" --clobber
done

cd "${tmp_dir}"
for asset in "${expected_assets[@]}"; do
  [[ "${asset}" == "SHA256SUMS" ]] && continue
  grep "  ${asset}$" SHA256SUMS | sha256_check
done

bash "${repo_root}/scripts/check-release-binary-compat.sh" linux-aarch64 ctx-linux-aarch64
bash "${repo_root}/scripts/check-release-binary-compat.sh" linux-x64 ctx-linux-x64
bash "${repo_root}/scripts/check-release-binary-compat.sh" macos-arm64 ctx-macos-arm64
bash "${repo_root}/scripts/check-release-binary-compat.sh" macos-x64 ctx-macos-x64 \
  "${macos_llvm_readobj}" "${macos_llvm_objdump}"
bash "${repo_root}/scripts/check-release-binary-compat.sh" windows-x64 ctx-windows-x64.exe

printf 'GitHub release assets ok: %s %s\n' "${repo}" "${tag}"
