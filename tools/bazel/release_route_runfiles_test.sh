#!/usr/bin/env bash
set -euo pipefail

: "${TEST_SRCDIR:?Bazel test runfiles are required}"
: "${TEST_WORKSPACE:?Bazel test workspace is required}"
: "${TEST_TMPDIR:?Bazel test temporary directory is required}"

run_route() {
  local route="$1"
  local launcher="${TEST_SRCDIR}/${TEST_WORKSPACE}/tools/bazel/${route}"
  local fixture="${TEST_TMPDIR}/${route}"
  test -x "${launcher}"
  test ! -e "${fixture}"
  test ! -e "${fixture}.runfiles"
  ln -s "${launcher}" "${fixture}"
  ln -s "${TEST_SRCDIR}" "${fixture}.runfiles"
  env -u RUNFILES_DIR -u RUNFILES_MANIFEST_FILE "${fixture}"
}

run_route _release_route_runfiles_probe
run_route _release_route_windows_runfiles_probe
