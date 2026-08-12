#!/usr/bin/env bash
set -euo pipefail

readonly SCRIPT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
readonly PYTHON_URL='https://github.com/astral-sh/python-build-standalone/releases/download/20240726/cpython-3.11.9%2B20240726-aarch64-apple-darwin-install_only.tar.gz'
readonly PYTHON_SHA256='cbdac9462bab9671c8e84650e425d3f43b775752a930a2ef954a0d457d5c00c3'
readonly UV_URL='https://github.com/astral-sh/uv/releases/download/0.8.9/uv-aarch64-apple-darwin.tar.gz'
readonly UV_SHA256='c233bee389c15fdef09a6028db61cc54a12e6171f27d6d9c018eedca5bbbd011'

die() {
  printf 'semantic Core ML toolchain error: %s\n' "$*" >&2
  exit 1
}

sha256() {
  shasum -a 256 "$1" | awk '{print $1}'
}

download_exact() {
  local url="$1"
  local expected="$2"
  local output="$3"
  curl --fail --location --proto '=https' --tlsv1.2 \
    --retry 3 --retry-all-errors --silent --show-error \
    --output "${output}" "${url}"
  local actual
  actual="$(sha256 "${output}")"
  [[ "${actual}" == "${expected}" ]] ||
    die "download digest mismatch for ${url}"
}

if [[ "${CTX_SEMANTIC_COREML_TOOLCHAIN_DRY_RUN:-0}" == '1' ]]; then
  printf 'python_url=%s\npython_sha256=%s\nuv_url=%s\nuv_sha256=%s\n' \
    "${PYTHON_URL}" "${PYTHON_SHA256}" "${UV_URL}" "${UV_SHA256}"
  exit 0
fi

[[ "$(uname -s)" == 'Darwin' ]] || die 'producer requires macOS'
[[ "$(uname -m)" == 'arm64' ]] || die 'producer requires native Apple Silicon'
[[ "$#" -gt 0 ]] || die 'producer arguments are required'

work_root="$(mktemp -d "${TMPDIR:-/tmp}/ctx-coreml-toolchain.XXXXXX")"
trap 'rm -rf -- "${work_root}"' EXIT

python_archive="${work_root}/python.tar.gz"
uv_archive="${work_root}/uv.tar.gz"
download_exact "${PYTHON_URL}" "${PYTHON_SHA256}" "${python_archive}"
download_exact "${UV_URL}" "${UV_SHA256}" "${uv_archive}"

tar -xzf "${python_archive}" -C "${work_root}"
tar -xzf "${uv_archive}" -C "${work_root}"
readonly PYTHON="${work_root}/python/bin/python3.11"
readonly UV="${work_root}/uv-aarch64-apple-darwin/uv"
[[ -x "${PYTHON}" && -x "${UV}" ]] || die 'pinned tool archive layout changed'
[[ "$(${PYTHON} -c 'import platform; print(platform.python_version())')" == '3.11.9' ]] ||
  die 'pinned Python reports the wrong version'

"${UV}" pip install \
  --python "${PYTHON}" \
  --exact \
  --no-cache \
  --requirements "${SCRIPT_ROOT}/requirements.lock"
"${UV}" pip check --python "${PYTHON}"

exec "${PYTHON}" "${SCRIPT_ROOT}/produce.py" "$@"
