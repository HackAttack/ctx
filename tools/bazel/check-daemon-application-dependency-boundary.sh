#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo 'usage: check-daemon-application-dependency-boundary.sh ROOT_BUILD' >&2
  exit 64
fi

root_build="$(readlink -f "$1")"
repo_root="$(dirname "${root_build}")"
tmp="$(mktemp -d "${TEST_TMPDIR:-/tmp}/ctx-daemon-application-boundary.XXXXXX")"
trap 'rm -rf -- "${tmp}"' EXIT
mkdir -p "${tmp}/home"

query() {
  env -u BUILD_WORKSPACE_DIRECTORY \
    HOME="${tmp}/home" \
    BAZEL_OUTPUT_USER_ROOT="${tmp}/bazel-output" \
    CTX_BAZEL_SANDBOX_BASE="${tmp}/bazel-sandboxes" \
    CTX_BAZEL_WORKSPACE="${repo_root}" \
    "${repo_root}/scripts/bazelw" query "$1" --output=label
}

expected_direct="${tmp}/expected-direct.txt"
printf '%s\n' \
  '//crates/ctx-client-observability:lib' \
  '//crates/ctx-daemon-application:lib' \
  '//crates/ctx-daemon-runtime:lib' \
  '//crates/ctx-daemon-service:lib' \
  '//crates/ctx-history-core:lib' >"${expected_direct}"
query 'kind("rust_library rule", deps(//crates/ctx-daemon-application:lib, 1)) intersect //crates/...' \
  | LC_ALL=C sort -u >"${tmp}/actual-direct.txt"
if ! diff -u "${expected_direct}" "${tmp}/actual-direct.txt"; then
  echo 'unexpected direct internal dependency set for ctx-daemon-application' >&2
  exit 1
fi

expected_qualification="${tmp}/expected-qualification.txt"
printf '%s\n' \
  '//crates/ctx-client-observability:lib' \
  '//crates/ctx-daemon-application:qualification_lib' \
  '//crates/ctx-daemon-runtime:qualification_lib' \
  '//crates/ctx-daemon-service:qualification_lib' \
  '//crates/ctx-history-core:lib' >"${expected_qualification}"
query 'kind("rust_library rule", deps(//crates/ctx-daemon-application:qualification_lib, 1)) intersect //crates/...' \
  | LC_ALL=C sort -u >"${tmp}/actual-qualification.txt"
if ! diff -u "${expected_qualification}" "${tmp}/actual-qualification.txt"; then
  echo 'unexpected qualification dependency set for ctx-daemon-application' >&2
  exit 1
fi

expected_test_support="${tmp}/expected-test-support.txt"
printf '%s\n' \
  '//crates/ctx-client-observability:test_support_lib' \
  '//crates/ctx-daemon-application:test_support_lib' \
  '//crates/ctx-daemon-runtime:lib' \
  '//crates/ctx-daemon-service:test_support_lib' \
  '//crates/ctx-history-core:lib' >"${expected_test_support}"
query 'kind("rust_library rule", deps(//crates/ctx-daemon-application:test_support_lib, 1)) intersect //crates/...' \
  | LC_ALL=C sort -u >"${tmp}/actual-test-support.txt"
if ! diff -u "${expected_test_support}" "${tmp}/actual-test-support.txt"; then
  echo 'unexpected test-support dependency set for ctx-daemon-application' >&2
  exit 1
fi

expected_reverse="${tmp}/expected-reverse.txt"
printf '%s\n' \
  '//crates/ctx-cli:ctx' \
  '//crates/ctx-cli:ctx_auto_upgrade_acceptance_fixture' \
  '//crates/ctx-cli:ctx_hosted_uninstall_test_host' \
  '//crates/ctx-cli:ctx_pro_test_host' \
  '//crates/ctx-cli:ctx_upgrade_test_harness' \
  '//crates/ctx-daemon-application:lib' \
  '//crates/ctx-daemon-cli:lib' \
  '//crates/ctx-daemon-cli:qualification_test_support_lib' \
  '//crates/ctx-history-cli:lib' >"${expected_reverse}"
query 'kind("rust_binary rule", rdeps(//crates/..., //crates/ctx-daemon-application:lib)) union kind("rust_library rule", rdeps(//crates/..., //crates/ctx-daemon-application:lib))' \
  | LC_ALL=C sort -u >"${tmp}/actual-reverse.txt"
if ! diff -u "${expected_reverse}" "${tmp}/actual-reverse.txt"; then
  echo 'ctx-daemon-application must have only CLI production consumers' >&2
  exit 1
fi

expected_reverse_qualification="${tmp}/expected-reverse-qualification.txt"
printf '%s\n' \
  '//crates/ctx-cli:ctx_auto_upgrade_acceptance_fixture' \
  '//crates/ctx-cli:ctx_hosted_uninstall_test_host' \
  '//crates/ctx-cli:ctx_upgrade_test_harness' \
  '//crates/ctx-daemon-application:qualification_lib' \
  '//crates/ctx-daemon-cli:qualification_lib' >"${expected_reverse_qualification}"
query 'kind("rust_binary rule", rdeps(//crates/..., //crates/ctx-daemon-application:qualification_lib)) union kind("rust_library rule", rdeps(//crates/..., //crates/ctx-daemon-application:qualification_lib))' \
  | LC_ALL=C sort -u >"${tmp}/actual-reverse-qualification.txt"
if ! diff -u "${expected_reverse_qualification}" "${tmp}/actual-reverse-qualification.txt"; then
  echo 'unexpected qualification consumer of ctx-daemon-application' >&2
  exit 1
fi

expected_reverse_test_support="${tmp}/expected-reverse-test-support.txt"
printf '%s\n' \
  '//crates/ctx-cli:unit_tests' \
  '//crates/ctx-daemon-application:test_support_lib' \
  '//crates/ctx-daemon-cli:test_support_lib' \
  '//crates/ctx-daemon-cli:unit_tests' \
  '//crates/ctx-history-cli:test_support_lib' >"${expected_reverse_test_support}"
query 'kind("rust_library rule", rdeps(//crates/..., //crates/ctx-daemon-application:test_support_lib)) union kind("rust_test rule", rdeps(//crates/..., //crates/ctx-daemon-application:test_support_lib))' \
  | LC_ALL=C sort -u >"${tmp}/actual-reverse-test-support.txt"
if ! diff -u "${expected_reverse_test_support}" "${tmp}/actual-reverse-test-support.txt"; then
  echo 'unexpected test-support consumer of ctx-daemon-application' >&2
  exit 1
fi

if [[ -n "$(query 'somepath(//crates/ctx-daemon-application:lib, //crates/ctx-cli:ctx)')" ]]; then
  echo 'ctx-daemon-application has a reverse dependency path into ctx-cli' >&2
  exit 1
fi

python3 - "${repo_root}" <<'PY'
import importlib.util
import pathlib
import sys
import tomllib

root = pathlib.Path(sys.argv[1])
manifest_path = root / "crates/ctx-daemon-application/Cargo.toml"
manifest = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
expected_dependencies = {
    "anyhow",
    "ctx-client-observability",
    "ctx-daemon-runtime",
    "ctx-daemon-service",
    "ctx-history-core",
    "serde_json",
    "sha2",
}
actual_dependencies = set(manifest.get("dependencies", {}))
if actual_dependencies != expected_dependencies:
    raise SystemExit(
        "ctx-daemon-application dependency inventory differs: "
        f"missing={sorted(expected_dependencies - actual_dependencies)} "
        f"extra={sorted(actual_dependencies - expected_dependencies)}"
    )
if set(manifest.get("dev-dependencies", {})) != {"tempfile"}:
    raise SystemExit("ctx-daemon-application dev dependency inventory differs")
if manifest.get("features"):
    raise SystemExit("ctx-daemon-application must not define feature-selected authority")

reverse = []
for candidate in sorted((root / "crates").glob("*/Cargo.toml")):
    if candidate != manifest_path and "ctx-daemon-application" in candidate.read_text(encoding="utf-8"):
        reverse.append(candidate.relative_to(root).as_posix())
if reverse != ["crates/ctx-daemon-cli/Cargo.toml"]:
    raise SystemExit(f"unexpected reverse Cargo consumer of ctx-daemon-application: {reverse}")

try:
    import tomli
except ModuleNotFoundError:
    sys.modules["tomli"] = tomllib
spec = importlib.util.spec_from_file_location(
    "check_rust_crate_size", root / "scripts/check-rust-crate-size.py"
)
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)
measurement = next(
    item for item in module.live_measurements(root)
    if item.package.name == "ctx-daemon-application"
)
if not 7_000 <= measurement.cloc <= 10_500:
    raise SystemExit(
        "ctx-daemon-application must remain within its 7,000-10,500 physical CLOC policy band: "
        f"{measurement.cloc}"
    )
physical = sum(
    len(path.read_text(encoding="utf-8").splitlines())
    for path in (root / "crates/ctx-daemon-application").rglob("*.rs")
)
if physical > 11_000:
    raise SystemExit(
        "ctx-daemon-application exceeds its 11,000 physical Rust hard stop: "
        f"{physical}"
    )
print(
    "ctx-daemon-application size boundary: "
    f"files={measurement.files} cloc={measurement.cloc} physical={physical}"
)
PY

application_root="${repo_root}/crates/ctx-daemon-application"
if find "${application_root}" -type l -print -quit | grep -q .; then
  echo 'ctx-daemon-application must contain no symlinked source or metadata' >&2
  exit 1
fi
if find "${application_root}" -name '*.rs' ! -path "${application_root}/src/*" -print -quit | grep -q .; then
  echo 'ctx-daemon-application Rust sources must remain package-local under src' >&2
  exit 1
fi
if grep -En 'ctx-(agent-integrations|cli|history-capture|history-index|history-query|history-refresh|pro-host|pro-lifecycle|protocol|semantic-index|semantic-model|upgrade-engine)([^[:alnum:]_-]|$)|(^|[^[:alnum:]_-])(clap|ureq)([^[:alnum:]_-]|$)' \
  "${application_root}/Cargo.toml"; then
  echo 'excluded product, presentation, network, semantic, or upgrade authority leaked into ctx-daemon-application' >&2
  exit 1
fi
if grep -REn --include='*.rs' \
  'ctx_(agent_integrations|history_capture|history_index|history_query|history_refresh|pro_host|pro_lifecycle|protocol|semantic_index|semantic_model|upgrade_engine)::|(^|[^[:alnum:]_])(clap|ureq)::|crate::(commands|config|net|output|pro|semantic|ui|upgrade)::|\b(AppConfig|Ui)\b' \
  "${application_root}/src"; then
  echo 'excluded implementation authority leaked into ctx-daemon-application source' >&2
  exit 1
fi
if grep -REn --include='*.rs' \
  'thread_local!|ACTIVE_HOST|RefCell[[:space:]]*<[[:space:]]*Option[[:space:]]*<[[:space:]]*Box|Box[[:space:]]*<[[:space:]]*dyn[[:space:]]+DaemonApplicationHost|Arc[[:space:]]*<[[:space:]]*dyn[[:space:]]+DaemonApplicationHost|Box::leak|#\[path|include!|(^|[^[:alnum:]_])unsafe([^[:alnum:]_]|$)' \
  "${application_root}/src"; then
  echo 'global host registry, lifetime escape, source indirection, or unsafe boundary leaked into ctx-daemon-application' >&2
  exit 1
fi

printf 'ctx-daemon-application dependency, locality, and borrowed-port boundary ok\n'
