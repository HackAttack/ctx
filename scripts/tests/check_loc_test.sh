#!/usr/bin/env bash
set -euo pipefail

readonly scripts_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly repository_root="$(cd "${scripts_dir}/.." && pwd)"
readonly checker_shell="${scripts_dir}/check-loc.sh"
readonly checker_python="${scripts_dir}/check-loc.py"
readonly policy_checker="${scripts_dir}/check-repository-policy.sh"
readonly inventory_checker="${repository_root}/tools/bazel/check_rust_target_inventory.py"
readonly test_root="$(mktemp -d "${TMPDIR:-/tmp}/ctx-loc-test.XXXXXX")"
current_repo=''

cleanup() {
  if [[ -n "${test_root}" && "${test_root}" == "${TMPDIR:-/tmp}"/ctx-loc-test.* && -d "${test_root}" ]]; then
    rm -rf -- "${test_root}"
  fi
}
trap cleanup EXIT

fail() {
  printf 'check_loc_test failed: %s\n' "$*" >&2
  exit 1
}

new_repo() {
  local name="$1"
  current_repo="${test_root}/${name}"
  mkdir -p "${current_repo}/scripts"
  git init -q "${current_repo}"
  cp "${checker_shell}" "${current_repo}/scripts/check-loc.sh"
  cp "${checker_python}" "${current_repo}/scripts/check-loc.py"
  chmod +x "${current_repo}/scripts/check-loc.sh"
  git -C "${current_repo}" add scripts
  git -C "${current_repo}" -c user.name=ctx-loc-test \
    -c user.email=ctx-loc-test@example.invalid commit -qm 'seed LOC gate'
}

write_lines() {
  local path="$1"
  local count="$2"
  mkdir -p "$(dirname "${path}")"
  awk -v count="${count}" 'BEGIN { for (i = 1; i <= count; i++) print "// line " i }' > "${path}"
}

run_gate() {
  local repo="$1"
  shift
  (cd "${repo}" && env "$@" bash scripts/check-loc.sh)
}

expect_pass() {
  local repo="$1"
  shift
  local output
  if ! output="$(run_gate "${repo}" "$@" 2>&1)"; then
    printf '%s\n' "${output}" >&2
    fail "expected LOC gate to pass in ${repo}"
  fi
}

expect_fail() {
  local repo="$1"
  local expected="$2"
  shift 2
  local output
  if output="$(run_gate "${repo}" "$@" 2>&1)"; then
    printf '%s\n' "${output}" >&2
    fail "expected LOC gate to fail in ${repo}"
  fi
  case "${output}" in
    *"${expected}"*) ;;
    *)
      printf '%s\n' "${output}" >&2
      fail "failure did not contain expected text: ${expected}"
      ;;
  esac
}

run_repository_policy() {
  local repo="$1"
  (cd "${repo}" && bash scripts/check-repository-policy.sh)
}

expect_repository_policy_fail() {
  local repo="$1"
  local expected="$2"
  local output
  if output="$(run_repository_policy "${repo}" 2>&1)"; then
    printf '%s\n' "${output}" >&2
    fail "expected repository policy to fail in ${repo}"
  fi
  case "${output}" in
    *"${expected}"*) ;;
    *)
      printf '%s\n' "${output}" >&2
      fail "repository-policy failure did not contain expected text: ${expected}"
      ;;
  esac
}

new_repo tracked
write_lines "${current_repo}/src/large.rs" 1001
git -C "${current_repo}" add src/large.rs
expect_fail "${current_repo}" 'src/large.rs (source): 1001 lines > limit 1000'

new_repo staged_new
write_lines "${current_repo}/src/staged.rs" 1001
git -C "${current_repo}" add src/staged.rs
expect_fail "${current_repo}" 'src/staged.rs (source): 1001 lines > limit 1000'

new_repo untracked
write_lines "${current_repo}/tests/new_test.rs" 1501
expect_fail "${current_repo}" 'tests/new_test.rs (test): 1501 lines > limit 1500'

new_repo ignored
printf 'ignored/\n' > "${current_repo}/.gitignore"
git -C "${current_repo}" add .gitignore
write_lines "${current_repo}/ignored/large.rs" 1001
expect_pass "${current_repo}"

new_repo source_symlink
readonly linked_source_root="${test_root}/linked-source-root"
write_lines "${linked_source_root}/bindgen.rs" 1001
mkdir -p "${current_repo}/src"
ln -s "${linked_source_root}/bindgen.rs" "${current_repo}/src/generated.rs"
expect_fail "${current_repo}" 'refusing to follow source path through a symlink: src/generated.rs'

new_repo generated_source
write_lines "${current_repo}/generated/large.rs" 1001
git -C "${current_repo}" add generated/large.rs
expect_fail "${current_repo}" 'generated/large.rs (source): 1001 lines > limit 1000'

new_repo tracked_output_named_directories
write_lines "${current_repo}/target/tracked.rs" 1001
write_lines "${current_repo}/bazel-generated/tracked.rs" 1001
git -C "${current_repo}" add -f target/tracked.rs bazel-generated/tracked.rs
expect_fail "${current_repo}" 'bazel-generated/tracked.rs (source): 1001 lines > limit 1000'
expect_fail "${current_repo}" 'target/tracked.rs (source): 1001 lines > limit 1000'

new_repo nested_production_names_are_source
write_lines "${current_repo}/src/docs/tracked.rs" 1001
write_lines "${current_repo}/src/fixtures/tracked.rs" 1001
git -C "${current_repo}" add src/docs/tracked.rs src/fixtures/tracked.rs
expect_fail "${current_repo}" 'src/docs/tracked.rs (source): 1001 lines > limit 1000'
expect_fail "${current_repo}" 'src/fixtures/tracked.rs (source): 1001 lines > limit 1000'

new_repo source_names_cannot_impersonate_docs
write_lines "${current_repo}/src/README.rs" 1001
write_lines "${current_repo}/src/CHANGELOG.py" 1001
write_lines "${current_repo}/src/upgrade.PS1" 1001
git -C "${current_repo}" add src/README.rs src/CHANGELOG.py src/upgrade.PS1
expect_fail "${current_repo}" 'src/README.rs (source): 1001 lines > limit 1000'
expect_fail "${current_repo}" 'src/CHANGELOG.py (source): 1001 lines > limit 1000'
expect_fail "${current_repo}" 'src/upgrade.PS1 (source): 1001 lines > limit 1000'

new_repo generated_data_source
write_lines "${current_repo}/data/generated/large.rs" 1001
git -C "${current_repo}" add data/generated/large.rs
expect_fail "${current_repo}" 'data/generated/large.rs (source): 1001 lines > limit 1000'

new_repo external_source
write_lines "${current_repo}/external/large.rs" 1001
git -C "${current_repo}" add external/large.rs
expect_fail "${current_repo}" 'external/large.rs (source): 1001 lines > limit 1000'

new_repo bazel_declarations
for declaration in BUILD BUILD.bazel WORKSPACE WORKSPACE.bazel MODULE.bazel; do
  write_lines "${current_repo}/${declaration}" 2001
done
git -C "${current_repo}" add BUILD BUILD.bazel WORKSPACE WORKSPACE.bazel MODULE.bazel
expect_pass "${current_repo}"
write_lines "${current_repo}/rules.bzl" 1001
git -C "${current_repo}" add rules.bzl
expect_fail "${current_repo}" 'rules.bzl (source): 1001 lines > limit 1000'

new_repo fixed_boundaries
write_lines "${current_repo}/src/at_limit.rs" 1000
write_lines "${current_repo}/tests/at_limit_test.rs" 1500
git -C "${current_repo}" add src/at_limit.rs tests/at_limit_test.rs
expect_pass "${current_repo}"
printf '// one line beyond the fixed limit\n' >> "${current_repo}/src/at_limit.rs"
expect_fail "${current_repo}" 'src/at_limit.rs (source): 1001 lines > limit 1000'

new_repo no_final_newline
mkdir -p "${current_repo}/src"
awk 'BEGIN { for (i = 1; i <= 1000; i++) printf "// line %d\n", i; printf "// final" }' \
  > "${current_repo}/src/no_final_newline.rs"
expect_fail "${current_repo}" 'src/no_final_newline.rs (source): 1001 lines > limit 1000'

new_repo non_source
write_lines "${current_repo}/docs/example.py" 2001
write_lines "${current_repo}/fixtures/example.rs" 2001
write_lines "${current_repo}/BUILD.bazel" 2001
git -C "${current_repo}" add docs/example.py fixtures/example.rs BUILD.bazel
expect_pass "${current_repo}"

new_repo repository_policy_subprocess_shadow
cp "${policy_checker}" "${current_repo}/scripts/check-repository-policy.sh"
mkdir -p "${current_repo}/tools/bazel"
cp "${inventory_checker}" "${current_repo}/tools/bazel/check_rust_target_inventory.py"
printf '# policy root\n' > "${current_repo}/BUILD.bazel"
printf "open('subprocess-shadow-executed', 'w', encoding='utf-8').write('executed')\nraise SystemExit(0)\n" \
  > "${current_repo}/scripts/subprocess.py"
write_lines "${current_repo}/src/large.rs" 1001
expect_repository_policy_fail "${current_repo}" 'src/large.rs (source): 1001 lines > limit 1000'
if [[ -e "${current_repo}/subprocess-shadow-executed" ]]; then
  fail 'candidate scripts/subprocess.py was imported by repository policy'
fi

new_repo bazel_execroot
printf 'bazel-*\n' > "${current_repo}/.gitignore"
git -C "${current_repo}" add .gitignore
readonly source_repo="${current_repo}"
readonly execroot="${test_root}/execroot"
readonly runfiles_scripts="${execroot}/bazel-out/k8-fastbuild/bin/loc_check.runfiles/_main/scripts"
mkdir -p "${runfiles_scripts}"
ln -s "${source_repo}/.git" "${execroot}/.git"
ln -s '.gitignore' "${execroot}/.gitignore"
cp "${checker_shell}" "${runfiles_scripts}/check-loc.sh"
cp "${checker_python}" "${runfiles_scripts}/check-loc.py"
chmod +x "${runfiles_scripts}/check-loc.sh"
if ! execroot_output="$(
  cd "${execroot}" &&
    CTX_LOC_REPO_ROOT="${execroot}" bash "${runfiles_scripts}/check-loc.sh" 2>&1
)"; then
  printf '%s\n' "${execroot_output}" >&2
  fail 'expected checker launched from Bazel runfiles to use the physical source root'
fi

printf 'check_loc_test passed\n'
