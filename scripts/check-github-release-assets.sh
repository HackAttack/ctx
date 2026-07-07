#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/check-github-release-assets.sh TAG [REPO]

Checks that a published GitHub Release has the expected ctx binary assets and
that SHA256SUMS verifies them. REPO defaults to ctxrs/ctx.
USAGE
}

tag="${1:-}"
repo="${2:-ctxrs/ctx}"

if [[ -z "${tag}" || "${tag}" == "-h" || "${tag}" == "--help" ]]; then
  usage
  exit 2
fi

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
  ctx-freebsd-x64
  ctx-linux-aarch64
  ctx-linux-x64
  ctx-macos-arm64
  ctx-macos-x64
  ctx-windows-x64.exe
  SHA256SUMS
)

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/ctx-github-release-assets.XXXXXX")"
trap 'rm -rf "${tmp_dir}"' EXIT

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
for asset in \
  ctx-linux-aarch64 \
  ctx-linux-x64 \
  ctx-macos-arm64 \
  ctx-macos-x64 \
  ctx-windows-x64.exe \
  ctx-freebsd-x64; do
  grep "  ${asset}$" SHA256SUMS | sha256_check
done

printf 'GitHub release assets ok: %s %s\n' "${repo}" "${tag}"
