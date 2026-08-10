#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${RUNFILES_DIR:-}" ]]; then
  root="${RUNFILES_DIR}/_main"
elif [[ -n "${TEST_SRCDIR:-}" ]]; then
  root="${TEST_SRCDIR}/_main"
else
  root="${BUILD_WORKSPACE_DIRECTORY:-$(cd "$(dirname "$0")/../.." && pwd)}"
fi

engine_build_rs="${root}/crates/ctx-upgrade-engine/build.rs"
engine_build="${root}/crates/ctx-upgrade-engine/BUILD.bazel"
engine_manifest="${root}/crates/ctx-upgrade-engine/Cargo.toml"
engine_sources="${root}/crates/ctx-upgrade-engine/src"
cli_build="${root}/crates/ctx-cli/BUILD.bazel"

for cfg in ctx_release_qualification ctx_upgrade_engine_test_support ctx_cli_bazel_test; do
  grep -F "cargo:rustc-check-cfg=cfg(${cfg})" "${engine_build_rs}" >/dev/null
  grep -F -- "--check-cfg=cfg(${cfg})" "${engine_build}" >/dev/null
done
grep -F '[features]' "${engine_manifest}" >/dev/null
grep -F 'test-support = []' "${engine_manifest}" >/dev/null

for target in lib test_support_lib qualification_lib unit_tests; do
  grep -F "name = \"${target}\"" "${engine_build}" >/dev/null
done
grep -F -- '--cfg=feature="test-support"' "${engine_build}" >/dev/null
grep -F -- '--cfg=ctx_release_qualification' "${engine_build}" >/dev/null
grep -F 'rustc_env_files = ["test-harness.env"]' "${engine_build}" >/dev/null

grep -F '"//crates/ctx-upgrade-engine:lib"' "${cli_build}" >/dev/null
grep -F '"//crates/ctx-upgrade-engine:test_support_lib"' "${cli_build}" >/dev/null
grep -F '"//crates/ctx-upgrade-engine:qualification_lib"' "${cli_build}" >/dev/null

if grep -REn --include='*.rs' 'env!\("CARGO_PKG_VERSION"\)' "${engine_sources}"; then
  echo 'ctx-upgrade-engine must receive product identity from composition' >&2
  exit 1
fi
if [[ -e "${root}/crates/ctx-cli/src/upgrade/test-harness.env" ]]; then
  echo 'upgrade qualification environment remains owned by ctx-cli' >&2
  exit 1
fi

printf 'ctx-upgrade-engine Cargo/Bazel cfg and qualification parity ok\n'
