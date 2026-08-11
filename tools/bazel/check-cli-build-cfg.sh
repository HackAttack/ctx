#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${RUNFILES_DIR:-}" ]]; then
  root="${RUNFILES_DIR}/_main"
elif [[ -n "${TEST_SRCDIR:-}" ]]; then
  root="${TEST_SRCDIR}/_main"
else
  root="${BUILD_WORKSPACE_DIRECTORY:-$(cd "$(dirname "$0")/../.." && pwd)}"
fi
build_rs="${root}/crates/ctx-cli/build.rs"
bazel_cfg="${root}/crates/ctx-cli/test_targets.bzl"
binary_contracts="${root}/tools/bazel/binary_contracts.bzl"

grep -F 'CTX_CLI_RUSTC_FLAGS = CTX_BINARY_CONTRACT_RUSTC_FLAGS' "${bazel_cfg}" >/dev/null

for cfg in ctx_cli_bazel_test; do
  grep -F "cargo:rustc-check-cfg=cfg(${cfg})" "${build_rs}" >/dev/null
  grep -F -- "--check-cfg=cfg(${cfg})" "${binary_contracts}" >/dev/null
  grep -F -- "--cfg=${cfg}" "${binary_contracts}" >/dev/null
done

printf 'ctx-cli build.rs/native Bazel cfg parity ok\n'
