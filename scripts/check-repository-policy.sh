#!/usr/bin/env bash
set -euo pipefail

readonly root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
python3 "${root}/scripts/check-loc.py" --root "${root}"
python3 "${root}/tools/bazel/check_rust_target_inventory.py"
