#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

export CTX_BOOTSTRAP_BAZELISK="${CTX_BOOTSTRAP_BAZELISK:-1}"
export CTX_BAZELISK_VERSION="${CTX_BAZELISK_VERSION:-v1.29.0}"
export CTX_RUST_TOOLCHAIN="${CTX_RUST_TOOLCHAIN:-1.97.1}"

check_args=("$@")
if (( "${#check_args[@]}" == 0 )); then
  check_args=(--mode=ci)
fi

release_mode=0
for (( index = 0; index < "${#check_args[@]}"; index++ )); do
  if [[ "${check_args[index]}" == "--mode=release" ]] \
    || { [[ "${check_args[index]}" == "--mode" ]] \
      && [[ "${check_args[index + 1]:-}" == "release" ]]; }; then
    release_mode=1
    break
  fi
done

init_buildkite_job_tool_env() {
  if [[ -z "${BUILDKITE_JOB_ID:-}" ]]; then
    return 0
  fi

  local base_tmp build_path job_slug tool_root
  local repo_root_resolved repository_cache_resolved repository_contents_resolved
  local qualified_path runner_task_root_resolved test_tmpdir_resolved tmpdir_resolved
  if (( release_mode == 1 )); then
    if [[ -z "${CTX_RUNNER_TASK_ROOT:-}" || "${CTX_RUNNER_TASK_ROOT}" != /* \
      || ! -d "${CTX_RUNNER_TASK_ROOT}" ]]; then
      printf 'Buildkite release tests require an absolute existing CTX_RUNNER_TASK_ROOT\n' >&2
      exit 64
    fi
    runner_task_root_resolved="$(cd "${CTX_RUNNER_TASK_ROOT}" && pwd -P)"
    base_tmp="${runner_task_root_resolved}/tmp"
  else
    base_tmp="${TMPDIR:-/tmp}"
  fi
  build_path="${BUILDKITE_BUILD_PATH:-${base_tmp}}"
  job_slug="${BUILDKITE_JOB_ID//[^A-Za-z0-9_.-]/_}"
  tool_root="${CTX_PUBLIC_CI_TOOL_ROOT:-${base_tmp}/ctx-public-ci-${job_slug}}"

  export TMPDIR="${CTX_PUBLIC_CI_TMPDIR:-${tool_root}/tmp}"
  export HOME="${CTX_PUBLIC_CI_HOME:-${tool_root}/home}"
  export CARGO_HOME="${CARGO_HOME:-${tool_root}/cargo-home}"
  export RUSTUP_HOME="${RUSTUP_HOME:-${tool_root}/rustup-home}"
  export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${tool_root}/cargo-target}"
  export CTX_TOOL_ENV_ROOT="${CTX_TOOL_ENV_ROOT:-${tool_root}/tool-env}"
  export BAZELISK_HOME="${BAZELISK_HOME:-${tool_root}/bazelisk-home}"
  export BAZEL_OUTPUT_USER_ROOT="${BAZEL_OUTPUT_USER_ROOT:-${tool_root}/bazel-output}"
  # Keep Bazel's downloaded and materialized repository contents in a
  # job-owned directory outside the source checkout. The job slug prevents
  # unsafe cross-job sharing while retaining reuse throughout this job.
  export CTX_PUBLIC_CI_REPOSITORY_CACHE="${CTX_PUBLIC_CI_REPOSITORY_CACHE:-${build_path%/}/ctx-public-ci-cache/${job_slug}/bazel-repository}"
  export CTX_BAZEL_REPOSITORY_CACHE="${CTX_BAZEL_REPOSITORY_CACHE:-${CTX_PUBLIC_CI_REPOSITORY_CACHE}}"
  if (( release_mode == 1 )); then
    export CTX_BAZEL_TEST_TMPDIR="${CTX_PUBLIC_CI_TEST_TMPDIR:-${tool_root}/bazel-test-tmp}"
  fi
  mkdir -p \
    "${TMPDIR}" \
    "${HOME}" \
    "${CARGO_HOME}" \
    "${RUSTUP_HOME}" \
    "${CARGO_TARGET_DIR}" \
    "${CTX_TOOL_ENV_ROOT}" \
    "${BAZELISK_HOME}" \
    "${BAZEL_OUTPUT_USER_ROOT}" \
    "${CTX_BAZEL_REPOSITORY_CACHE}/contents"
  if (( release_mode == 1 )); then
    mkdir -p "${CTX_BAZEL_TEST_TMPDIR}"
    tmpdir_resolved="$(cd "${TMPDIR}" && pwd -P)"
    test_tmpdir_resolved="$(cd "${CTX_BAZEL_TEST_TMPDIR}" && pwd -P)"
    for qualified_path in "${tmpdir_resolved}" "${test_tmpdir_resolved}"; do
      case "${qualified_path}/" in
        "${runner_task_root_resolved}/"*) ;;
        *)
          printf 'Buildkite release test temporary path escaped task-local bind: %s\n' \
            "${qualified_path}" >&2
          exit 64
          ;;
      esac
    done
  fi

  repo_root_resolved="$(cd "${repo_root}" && pwd -P)"
  repository_cache_resolved="$(cd "${CTX_BAZEL_REPOSITORY_CACHE}" && pwd -P)"
  repository_contents_resolved="$(cd "${CTX_BAZEL_REPOSITORY_CACHE}/contents" && pwd -P)"
  case "${repository_contents_resolved}/" in
    "${repo_root_resolved}/"*)
      printf 'Buildkite Bazel repository contents cache must be outside checkout: %s (checkout: %s)\n' \
        "${repository_contents_resolved}" "${repo_root_resolved}" >&2
      exit 64
      ;;
  esac
  export CTX_BAZEL_REPOSITORY_CACHE="${repository_cache_resolved}"
  printf 'Buildkite job tool root: %s\n' "${tool_root}"
  printf 'Buildkite Bazel repository cache: %s\n' "${CTX_BAZEL_REPOSITORY_CACHE}"
  printf 'Buildkite Bazel repository contents cache: %s\n' "${repository_contents_resolved}"
  if (( release_mode == 1 )); then
    printf 'Buildkite release Bazel test TMPDIR: %s\n' "${CTX_BAZEL_TEST_TMPDIR}"
  fi
}

preflight_release_test_authority() {
  if (( release_mode == 0 )); then
    return 0
  fi

  python3 - "${CTX_BAZEL_TEST_TMPDIR}" <<'PY'
import ctypes
import os
import pathlib
import sys
import tempfile

clone_fs = 0x00000200
libc = ctypes.CDLL(None, use_errno=True)
if libc.unshare(clone_fs) != 0:
    error = ctypes.get_errno()
    raise SystemExit(
        "Buildkite release tests require unshare(CLONE_FS) authority: "
        f"{os.strerror(error)} (errno {error})"
    )

temporary_root = pathlib.Path(sys.argv[1])
with tempfile.NamedTemporaryFile(dir=temporary_root) as probe:
    if pathlib.Path(probe.name).parent != temporary_root:
        raise SystemExit("release test TMPDIR probe escaped its task-local root")
print("Buildkite release test authority: task-local TMPDIR and unshare(CLONE_FS) available")
PY
}

run_apt_get() {
  if command -v sudo >/dev/null 2>&1; then
    sudo "$@"
  else
    "$@"
  fi
}

install_ubuntu_tools() {
  local required_packages=(
    build-essential \
    ca-certificates \
    curl \
    dbus-daemon \
    default-jdk-headless \
    dotnet-sdk-8.0 \
    git \
    jq \
    nodejs \
    npm \
    openssl \
    pkg-config \
    python3 \
    python3-build \
    python3-pip \
    python3-venv \
    ripgrep \
    ruby \
    unzip \
    zip
  )
  local missing_packages=()
  local package
  for package in "${required_packages[@]}"; do
    if [[ "${package}" == "npm" ]] && command -v npm >/dev/null 2>&1; then
      continue
    fi
    if ! dpkg-query -W -f='${Status}\n' "${package}" 2>/dev/null \
      | grep -Fqx 'install ok installed'; then
      missing_packages+=("${package}")
    fi
  done

  if (( "${#missing_packages[@]}" == 0 )); then
    printf 'Buildkite hosted Linux tool packages already installed\n'
    return 0
  fi

  command -v apt-get >/dev/null 2>&1 || {
    printf 'apt-get is required to install missing Buildkite tools: %s\n' \
      "${missing_packages[*]}" >&2
    exit 127
  }

  printf 'Installing missing Buildkite tool packages: %s\n' "${missing_packages[*]}"
  run_apt_get apt-get -o DPkg::Lock::Timeout=300 update
  run_apt_get env DEBIAN_FRONTEND=noninteractive apt-get \
    -o DPkg::Lock::Timeout=300 install -y --no-install-recommends \
    "${missing_packages[@]}"
}

configure_bazelisk() {
  mkdir -p "${HOME}/.local/bin"
  printf 'common --repository_cache=%s\n' "${CTX_BAZEL_REPOSITORY_CACHE}" > "${HOME}/.bazelrc"

  # shellcheck source=scripts/ci-common.sh
  source scripts/ci-common.sh
  bazelisk_path="$(ctx_bootstrap_bazelisk)"
  ln -sf "${bazelisk_path}" "${HOME}/.local/bin/bazelisk"
  ln -sf "${bazelisk_path}" "${HOME}/.local/bin/bazel"
  export PATH="${HOME}/.local/bin:${PATH}"
  bazelisk version
}

print_tool_versions() {
  bazelisk version
  python3 --version
  node --version
  npm --version
  javac -version
  java -version
  dotnet --info
  ruby --version
  jq --version
  rg --version
  openssl version
  zip --version
}

init_buildkite_job_tool_env
preflight_release_test_authority
install_ubuntu_tools
configure_bazelisk
print_tool_versions
bash scripts/check-sdks.sh --groups=contracts,typescript,python,go,jvm,dotnet --required-groups=contracts,typescript,python,go,jvm,dotnet
# Rust SDK compilation and tests remain authoritative native targets in every
# check.sh mode; the direct gate above owns the other Linux SDK toolchains,
# including the Linux-specific .NET process-tree implementation.
bash scripts/check.sh "${check_args[@]}"
