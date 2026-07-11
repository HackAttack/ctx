#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/prepare-linux-release-inputs.sh PLATFORM TARGET PREPARED_DIR

Fetches the Cargo inputs locked by Cargo.lock. This is the only networked
phase of Linux release construction.
USAGE
}

platform="${1:-}"
target="${2:-}"
prepared_dir="${3:-}"
if [[ -z "${platform}" || -z "${target}" || -z "${prepared_dir}" ]]; then
  usage
  exit 2
fi

case "${platform}:${target}:$(uname -m)" in
  linux-x64:x86_64-unknown-linux-gnu:x86_64|linux-x64:x86_64-unknown-linux-gnu:amd64) ;;
  linux-aarch64:aarch64-unknown-linux-gnu:aarch64|linux-aarch64:aarch64-unknown-linux-gnu:arm64) ;;
  *)
    printf 'release input preparation requires a matching native Linux host: %s %s %s\n' \
      "${platform}" "${target}" "$(uname -m)" >&2
    exit 1
    ;;
esac

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

if [[ -e "${prepared_dir}" ]] && [[ -n "$(find "${prepared_dir}" -mindepth 1 -print -quit)" ]]; then
  printf 'prepared input directory must start empty: %s\n' "${prepared_dir}" >&2
  exit 1
fi

mkdir -p "${prepared_dir}/cargo-home"
export CARGO_HOME="${prepared_dir}/cargo-home"
unset CARGO_BUILD_TARGET CARGO_ENCODED_RUSTFLAGS RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER

cargo fetch --locked --target "${target}"
printf 'prepared locked Linux release inputs: %s %s\n' "${platform}" "${target}"
