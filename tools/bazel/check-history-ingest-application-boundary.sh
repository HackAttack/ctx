#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo 'usage: check-history-ingest-application-boundary.sh ROOT_BUILD' >&2
  exit 64
fi

root_build="$(readlink -f "$1")"
repo_root="$(dirname "${root_build}")"
scratch="$(mktemp -d "${TEST_TMPDIR:-/tmp}/ctx-history-ingest-application-boundary.XXXXXX")"
trap 'rm -rf -- "${scratch}"' EXIT
mkdir -p "${scratch}/home"

query() {
  env -u BUILD_WORKSPACE_DIRECTORY \
    HOME="${scratch}/home" \
    BAZEL_OUTPUT_USER_ROOT="${scratch}/bazel-output" \
    CTX_BAZEL_SANDBOX_BASE="${scratch}/bazel-sandboxes" \
    CTX_BAZEL_WORKSPACE="${repo_root}" \
    "${repo_root}/scripts/bazelw" query "$1" --output=label
}

check_labels() {
  local description="$1"
  local expression="$2"
  local expected="$3"
  query "${expression}" | LC_ALL=C sort -u >"${scratch}/actual.txt"
  if ! diff -u "${expected}" "${scratch}/actual.txt"; then
    echo "unexpected ${description} for ctx-history-ingest-application" >&2
    exit 1
  fi
}

check_contract_inventory() {
  local package="$1"
  local expected="$2"
  query "kind(\"rust_test rule\", //crates/${package}:all)" | LC_ALL=C sort -u >"${scratch}/actual-contracts.txt"
  if ! diff -u "${expected}" "${scratch}/actual-contracts.txt"; then
    echo "ctx-${package} final-binary contract inventory drifted" >&2
    exit 1
  fi

  query "kind(\"rust_test rule\", //crates/${package}:all) intersect attr(\"data\", \".*ctx-cli:ctx.*\", //crates/${package}:all)" \
    | LC_ALL=C sort -u >"${scratch}/actual-binary-data-contracts.txt"
  sed '/:unit_tests$/d' "${expected}" >"${scratch}/expected-binary-data-contracts.txt"
  if ! diff -u "${scratch}/expected-binary-data-contracts.txt" "${scratch}/actual-binary-data-contracts.txt"; then
    echo "ctx-${package} contracts must receive final ctx only as data" >&2
    exit 1
  fi

  if [[ -n "$(query "kind(\"rust_test rule\", //crates/${package}:all) intersect attr(\"deps\", \".*ctx-cli:ctx.*\", //crates/${package}:all)")" ]]; then
    echo "ctx-${package} contracts must not compile against final ctx" >&2
    exit 1
  fi
  if [[ -n "$(query "kind(\"rust_test rule\", //crates/${package}:all) intersect attr(\"deps\", \".*//crates/(ctx-cli|ctx-cli-contract-tests|ctx-cli-presentation)(:|/).*\", //crates/${package}:all)")" ]]; then
    echo "ctx-${package} contracts retain a ctx or shared-support Rust backedge" >&2
    exit 1
  fi
  if [[ -n "$(query "kind(\"rust_test rule\", //crates/${package}:all) intersect attr(\"deps\", \".*//crates/ctx-[^:]*shared[^:]*(:|/).*\", //crates/${package}:all)")" ]]; then
    echo "ctx-${package} contracts retain a ctx or shared-support Rust backedge" >&2
    exit 1
  fi
}

printf '%s\n' \
  '//crates/ctx-history-capture-model:lib' \
  '//crates/ctx-history-core:lib' \
  '//crates/ctx-history-ingest-application:lib' \
  '//crates/ctx-history-refresh:lib' \
  '//crates/ctx-history-source-discovery:lib' \
  '//crates/ctx-history-source-io:lib' >"${scratch}/expected-direct.txt"
check_labels \
  'normal direct dependency set' \
  'kind("rust_library rule", deps(//crates/ctx-history-ingest-application:lib, 1)) intersect //crates/...' \
  "${scratch}/expected-direct.txt"

sed 's#ctx-history-ingest-application:lib#ctx-history-ingest-application:qualification_lib#' \
  "${scratch}/expected-direct.txt" >"${scratch}/expected-qualification.txt"
check_labels \
  'qualification direct dependency set' \
  'kind("rust_library rule", deps(//crates/ctx-history-ingest-application:qualification_lib, 1)) intersect //crates/...' \
  "${scratch}/expected-qualification.txt"

sed \
  -e 's#ctx-history-ingest-application:lib#ctx-history-ingest-application:test_support_lib#' \
  -e 's#ctx-history-refresh:lib#ctx-history-refresh:test_support_lib#' \
  "${scratch}/expected-direct.txt" >"${scratch}/expected-test-support.txt"
check_labels \
  'test-support direct dependency set' \
  'kind("rust_library rule", deps(//crates/ctx-history-ingest-application:test_support_lib, 1)) intersect //crates/...' \
  "${scratch}/expected-test-support.txt"

printf '%s\n' \
  '//crates/ctx-cli-presentation:lib' \
  '//crates/ctx-cli-presentation:qualification_lib' \
  '//crates/ctx-cli:ctx' \
  '//crates/ctx-cli:ctx_auto_upgrade_acceptance_fixture' \
  '//crates/ctx-cli:ctx_hosted_uninstall_test_host' \
  '//crates/ctx-cli:ctx_pro_test_host' \
  '//crates/ctx-cli:ctx_upgrade_test_harness' \
  '//crates/ctx-history-cli:lib' \
  '//crates/ctx-history-ingest-application:lib' >"${scratch}/expected-reverse.txt"
check_labels \
  'normal library consumer set' \
  'kind("rust_binary rule", rdeps(//crates/..., //crates/ctx-history-ingest-application:lib)) union kind("rust_library rule", rdeps(//crates/..., //crates/ctx-history-ingest-application:lib))' \
  "${scratch}/expected-reverse.txt"

printf '%s\n' \
  '//crates/ctx-history-ingest-application:qualification_lib' >"${scratch}/expected-reverse-qualification.txt"
check_labels \
  'isolated qualification consumer set' \
  'kind("rust_binary rule", rdeps(//crates/..., //crates/ctx-history-ingest-application:qualification_lib)) union kind("rust_library rule", rdeps(//crates/..., //crates/ctx-history-ingest-application:qualification_lib))' \
  "${scratch}/expected-reverse-qualification.txt"

printf '%s\n' \
  '//crates/ctx-cli-presentation:test_support_lib' \
  '//crates/ctx-cli-presentation:unit_tests' \
  '//crates/ctx-cli:unit_tests' \
  '//crates/ctx-history-cli:test_support_lib' \
  '//crates/ctx-history-ingest-application:test_support_lib' >"${scratch}/expected-reverse-test-support.txt"
check_labels \
  'test-support consumer set' \
  'kind("rust_library rule", rdeps(//crates/..., //crates/ctx-history-ingest-application:test_support_lib)) union kind("rust_test rule", rdeps(//crates/..., //crates/ctx-history-ingest-application:test_support_lib))' \
  "${scratch}/expected-reverse-test-support.txt"

printf '%s\n' \
  '//crates/ctx-history-ingest-application:custom_root_discovery_tests' \
  '//crates/ctx-history-ingest-application:explicit_path_revision_upgrade_tests' \
  '//crates/ctx-history-ingest-application:history_source_plugins_tests' \
  '//crates/ctx-history-ingest-application:import_change_reporting_tests' \
  '//crates/ctx-history-ingest-application:self_contained_core_content_tests' \
  '//crates/ctx-history-ingest-application:unit_tests' >"${scratch}/expected-contracts.txt"
check_contract_inventory 'ctx-history-ingest-application' "${scratch}/expected-contracts.txt"

python3 - "${repo_root}" <<'PY'
import pathlib
import sys
import tomllib

root = pathlib.Path(sys.argv[1])
manifest_path = root / "crates/ctx-history-ingest-application/Cargo.toml"
manifest = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
expected = {
    "anyhow",
    "ctx-history-capture-model",
    "ctx-history-core",
    "ctx-history-refresh",
    "ctx-history-source-discovery",
    "ctx-history-source-io",
    "serde",
    "serde_json",
    "sha2",
}
actual = set(manifest.get("dependencies", {}))
if actual != expected:
    raise SystemExit(
        "ctx-history-ingest-application dependency inventory differs: "
        f"missing={sorted(expected - actual)} extra={sorted(actual - expected)}"
    )
if set(manifest.get("dev-dependencies", {})) != {"ctx-history-refresh", "tempfile"}:
    raise SystemExit("ctx-history-ingest-application dev dependency inventory differs")
if manifest.get("features"):
    raise SystemExit("ctx-history-ingest-application must not define feature-selected authority")

reverse = []
for candidate in sorted((root / "crates").glob("*/Cargo.toml")):
    if candidate != manifest_path and "ctx-history-ingest-application" in candidate.read_text(encoding="utf-8"):
        reverse.append(candidate.relative_to(root).as_posix())
if reverse != ["crates/ctx-cli/Cargo.toml", "crates/ctx-history-cli/Cargo.toml"]:
    raise SystemExit(f"unexpected reverse Cargo consumer: {reverse}")
PY

application_root="${repo_root}/crates/ctx-history-ingest-application"
if find "${application_root}" -type l -print -quit | grep -q .; then
  echo 'ctx-history-ingest-application must contain no symlinked source or metadata' >&2
  exit 1
fi
if grep -En 'ctx-(agent|cli|daemon|history-capture|history-query|terminal|client-observability)([^[:alnum:]_-]|$)|(^|[^[:alnum:]_-])clap([^[:alnum:]_-]|$)' \
  "${application_root}/Cargo.toml"; then
  echo 'excluded provider, sibling application, daemon, query, presentation, or telemetry dependency leaked into ingest application' >&2
  exit 1
fi
if grep -REn --include='*.rs' \
  'ctx_(agent|cli|daemon|history_capture|history_query|terminal|client_observability)::|(^|[^[:alnum:]_])clap::|crate::(commands|config|output|semantic|ui)::|\b(AppConfig|Ui)\b' \
  "${application_root}/src"; then
  echo 'excluded implementation authority leaked into ingest application source' >&2
  exit 1
fi
if grep -REn --include='*.rs' \
  'std::process::Command|thread::spawn|tokio::spawn|rayon::spawn|dyn[[:space:]]+Fn|#\[path|include!|unsafe[[:space:]]+(fn|impl|trait|extern|\{)' \
  "${application_root}/src"; then
  echo 'process, thread, callback, source indirection, or unsafe authority leaked into ingest application' >&2
  exit 1
fi

for port in SourceDiscoveryPort CaptureAdmissionPort IngestRefreshPort IngestProgressPort; do
  if [[ "$(grep -Rhc --include='*.rs' "trait ${port}" "${application_root}/src" | awk '{sum += $1} END {print sum}')" -ne 1 ]]; then
    echo "ingest application must define exactly one ${port}" >&2
    exit 1
  fi
done

python3 - "${repo_root}" <<'PY'
import importlib.util
import pathlib
import sys
import tomllib

root = pathlib.Path(sys.argv[1])
sys.modules.setdefault("tomli", tomllib)
spec = importlib.util.spec_from_file_location(
    "check_rust_crate_size", root / "scripts/check-rust-crate-size.py"
)
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)
measurement = next(
    item for item in module.live_measurements(root)
    if item.package.name == "ctx-history-ingest-application"
)
if measurement.cloc > 6_089:
    raise SystemExit(
        "ctx-history-ingest-application exceeds its 6,089 physical CLOC ceiling: "
        f"{measurement.cloc}"
    )
print(f"ctx-history-ingest-application physical CLOC: {measurement.cloc}")
PY

printf 'ctx-history-ingest-application normal, qualification, test-support, contract, locality, CLOC, and borrowed-port boundary ok\n'
