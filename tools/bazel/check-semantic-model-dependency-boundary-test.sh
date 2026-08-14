#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo 'usage: check-semantic-model-dependency-boundary-test.sh CHECKER ROOT_BUILD' >&2
  exit 64
fi

checker="$(readlink -f "$1")"
root_build="$(readlink -f "$2")"
tmp="$(mktemp -d "${TEST_TMPDIR:-/tmp}/ctx-semantic-model-boundary-test.XXXXXX")"
trap 'rm -rf -- "${tmp}"' EXIT
fixture="${tmp}/fixture"

reset_fixture() {
  rm -rf -- "${fixture}"
  mkdir -p \
    "${fixture}/scripts" \
    "${fixture}/crates/ctx-semantic-model/src" \
    "${fixture}/crates/ctx-daemon-cli/src" \
    "${fixture}/crates/ctx-daemon-cli/src/tests" \
    "${fixture}/crates/ctx-daemon-service/src" \
    "${fixture}/crates/ctx-daemon-service/src/tests"

  : >"${fixture}/BUILD.bazel"
  : >"${fixture}/crates/ctx-semantic-model/Cargo.toml"
  : >"${fixture}/crates/ctx-semantic-model/src/lib.rs"
  cat >"${fixture}/crates/ctx-daemon-cli/src/daemon_service_ports.rs" <<'RS'
impl ArtifactFetcher for CliDaemonArtifactFetcher {}
RS
  cat >"${fixture}/crates/ctx-daemon-cli/src/tests/artifact_fetcher.rs" <<'RS'
impl ArtifactFetcher for TestArtifactFetcher {}
RS
  cat >"${fixture}/crates/ctx-daemon-service/src/daemon_worker.rs" <<'RS'
fn acquire(runtime: Runtime) {
    runtime.acquire_for_daemon();
}
RS
  cat >"${fixture}/crates/ctx-daemon-service/src/tests/acquisition.rs" <<'RS'
fn test_acquire(runtime: Runtime) {
    runtime.acquire_for_daemon();
}
RS
  cat >"${fixture}/scripts/bazelw" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

if [[ "$1" != query || "$#" -ne 3 || "$3" != --output=label ]]; then
  echo "unexpected fake bazel invocation: $*" >&2
  exit 2
fi

case "$2" in
  'kind("rust_library rule", deps(//crates/ctx-semantic-model:lib)) intersect //crates/...')
    printf '%s\n' \
      '//crates/ctx-history-platform:lib' \
      '//crates/ctx-semantic-model:lib'
    ;;
  'kind("rust_library rule", deps(//crates/ctx-semantic-model:test_support_lib)) intersect //crates/...')
    printf '%s\n' \
      '//crates/ctx-history-platform:lib' \
      '//crates/ctx-semantic-model:test_support_lib'
    ;;
  'somepath(//crates/ctx-semantic-model:lib, //crates/ctx-history-index:lib)') ;;
  'somepath(//crates/ctx-history-index:lib, //crates/ctx-semantic-model:lib)' | \
  'somepath(//crates/ctx-cli:ctx, //crates/ctx-semantic-model:lib)')
    printf '%s\n' '//crates/ctx-semantic-model:lib'
    ;;
  *)
    echo "unexpected fake query: $2" >&2
    exit 2
    ;;
esac
SH
  chmod +x "${fixture}/scripts/bazelw"
}

run_checker() {
  "${checker}" "${fixture}/BUILD.bazel" >"${tmp}/stdout" 2>"${tmp}/stderr"
}

expect_rejected() {
  local diagnostic="$1"

  if run_checker; then
    echo 'semantic model boundary checker accepted a contract mutation' >&2
    exit 1
  fi
  if ! grep -Fq "${diagnostic}" "${tmp}/stderr"; then
    cat "${tmp}/stderr" >&2
    echo "missing expected diagnostic: ${diagnostic}" >&2
    exit 1
  fi
}

"${checker}" "${root_build}"

reset_fixture
run_checker
grep -Fq 'ctx-semantic-model dependency and fetch-capability boundary ok' "${tmp}/stdout"

reset_fixture
rm "${fixture}/crates/ctx-daemon-cli/src/daemon_service_ports.rs"
mkdir -p "${fixture}/crates/ctx-cli/src/semantic"
printf '%s\n' 'impl ArtifactFetcher for StaleCliFetcher {}' \
  >"${fixture}/crates/ctx-cli/src/semantic/daemon_service_ports.rs"
expect_rejected 'the ctx-daemon-cli service adapter must be the sole production ArtifactFetcher implementation in ctx-daemon-cli'

reset_fixture
printf '%s\n' 'impl ArtifactFetcher for DuplicateFetcher {}' \
  >"${fixture}/crates/ctx-daemon-cli/src/duplicate_fetcher.rs"
expect_rejected 'the ctx-daemon-cli service adapter must be the sole production ArtifactFetcher implementation in ctx-daemon-cli'

reset_fixture
: >"${fixture}/crates/ctx-daemon-service/src/daemon_worker.rs"
expect_rejected 'ctx-daemon-service must have exactly one production daemon acquisition call'

reset_fixture
printf '%s\n' 'fn duplicate(runtime: Runtime) { runtime.acquire_for_daemon(); }' \
  >>"${fixture}/crates/ctx-daemon-service/src/daemon_worker.rs"
expect_rejected 'ctx-daemon-service must have exactly one production daemon acquisition call'

printf 'semantic model dependency boundary mutations rejected\n'
