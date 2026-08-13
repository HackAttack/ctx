#!/usr/bin/env bash
set -euo pipefail

ctx_bin="${1:?missing ctx binary path}"
manifest="${2:?missing ctx Cargo.toml path}"
workspace_manifest="${3:?missing workspace Cargo.toml path}"

grep -Eq '^version\.workspace[[:space:]]*=[[:space:]]*true[[:space:]]*$' "${manifest}" || {
  printf 'ctx package does not inherit its workspace version: %s\n' "${manifest}" >&2
  exit 1
}
version="$(awk -F '"' '
  /^\[workspace\.package\][[:space:]]*$/ { in_package=1; next }
  /^\[/ { in_package=0 }
  in_package && /^version[[:space:]]*=/ { print $2; exit }
' "${workspace_manifest}")"
if [[ -z "${version}" ]]; then
  printf 'could not read ctx workspace version from %s\n' "${workspace_manifest}" >&2
  exit 1
fi

actual="$("${ctx_bin}" --version)"
expected="ctx ${version}"
if [[ "${actual}" != "${expected}" ]]; then
  printf 'unexpected ctx --version output: got %q, want %q\n' "${actual}" "${expected}" >&2
  exit 1
fi
