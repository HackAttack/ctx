#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${1:-}" ]]; then
  readonly root_build="$(readlink -f "$1")"
  readonly root="$(dirname "${root_build}")"
else
  readonly root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fi
python3 "${root}/scripts/check-loc.py" --root "${root}"
python3 "${root}/tools/bazel/check_rust_target_inventory.py"
