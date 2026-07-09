#!/usr/bin/env bash
set -euo pipefail

LINUX_GLIBC_MAX_VERSION="2.39"

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/check-release-binary-compat.sh PLATFORM BINARY

Checks platform compatibility constraints for a public ctx release binary.
Platforms: linux-x64, linux-aarch64, macos-arm64, macos-x64, windows-x64,
freebsd-x64.
USAGE
}

platform="${1:-}"
binary="${2:-}"
if [[ -z "${platform}" || -z "${binary}" || "${platform}" == "-h" || "${platform}" == "--help" ]]; then
  usage
  exit 2
fi

if [[ ! -f "${binary}" ]]; then
  printf 'release binary missing: %s\n' "${binary}" >&2
  exit 1
fi

version_le() {
  local lhs="$1"
  local rhs="$2"
  [[ "$(printf '%s\n%s\n' "${lhs}" "${rhs}" | sort -V | tail -n 1)" == "${rhs}" ]]
}

max_symbol_version() {
  local prefix="$1"
  local path="$2"

  readelf --version-info "${path}" \
    | { grep -oE "${prefix}_[0-9]+\\.[0-9]+(\\.[0-9]+)?" || true; } \
    | sed "s/^${prefix}_//" \
    | sort -Vu \
    | tail -n 1
}

check_linux_elf() {
  if ! command -v readelf >/dev/null 2>&1; then
    printf 'readelf is required for Linux release compatibility checks\n' >&2
    exit 127
  fi

  local glibc_version
  glibc_version="$(max_symbol_version GLIBC "${binary}")"
  if [[ -z "${glibc_version}" ]]; then
    printf 'could not determine GLIBC requirement for %s\n' "${binary}" >&2
    exit 1
  fi

  if ! version_le "${glibc_version}" "${LINUX_GLIBC_MAX_VERSION}"; then
    printf 'Linux release binary requires GLIBC_%s, above supported floor GLIBC_%s: %s\n' \
      "${glibc_version}" "${LINUX_GLIBC_MAX_VERSION}" "${binary}" >&2
    exit 1
  fi

}

case "${platform}" in
  linux-x64|linux-aarch64)
    check_linux_elf
    ;;
  macos-arm64|macos-x64|windows-x64|freebsd-x64)
    ;;
  *)
    usage
    exit 2
    ;;
esac

printf 'release binary compatibility ok: %s %s\n' "${platform}" "${binary}"
