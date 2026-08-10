#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${RUNFILES_DIR:-}" ]]; then
  root="${RUNFILES_DIR}/_main"
elif [[ -n "${TEST_SRCDIR:-}" ]]; then
  root="${TEST_SRCDIR}/_main"
else
  root="${BUILD_WORKSPACE_DIRECTORY:-$(cd "$(dirname "$0")/../.." && pwd)}"
fi

build_rs="${root}/crates/ctx-history-capture/build.rs"
bazel_build="${root}/crates/ctx-history-capture/BUILD.bazel"
cfg="ctx_codex_causal_qualification"
environment="CTX_CODEX_CAUSAL_QUALIFICATION_BUILD"

grep -F "cargo:rustc-check-cfg=cfg(${cfg})" "${build_rs}" >/dev/null
grep -F "cargo:rerun-if-env-changed=${environment}" "${build_rs}" >/dev/null
grep -F "std::env::var(\"${environment}\")" "${build_rs}" >/dev/null
grep -F "cargo:rustc-cfg=${cfg}" "${build_rs}" >/dev/null

# Bazel's native unit target compiles every capture source with cfg(test),
# which selects the same qualification-only branches without reproducing the
# Cargo environment switch in production builds.
grep -F 'RUST_SRCS = glob(["src/**/*.rs"])' "${bazel_build}" >/dev/null
grep -F 'name = "unit_tests"' "${bazel_build}" >/dev/null
grep -F 'srcs = RUST_SRCS' "${bazel_build}" >/dev/null

printf 'ctx-history-capture build.rs/native Bazel cfg parity ok\n'
