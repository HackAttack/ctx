#!/usr/bin/env bash
set -euo pipefail

platform="${1:-}"
host_system="${2:-}"
host_arch="${3:-}"
runtime_status="${4:-}"

case "${runtime_status}" in
  not_run) printf 'not_run\n' ;;
  passed)
    if [[ "${platform}:${host_system}:${host_arch}" == "macos-x64:Darwin:arm64" ]]; then
      printf 'non_authoritative\n'
    else
      printf 'authoritative\n'
    fi
    ;;
  *)
    echo "runtime status must be passed or not_run" >&2
    exit 2
    ;;
esac
