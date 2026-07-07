#!/usr/bin/env bash
set -euo pipefail

ctx_bin="${1:?missing ctx binary path}"
manifest="${2:?missing ctx Cargo.toml path}"

version="$(awk -F '"' '/^version[[:space:]]*=/ { print $2; exit }' "${manifest}")"
if [[ -z "${version}" ]]; then
  printf 'could not read ctx package version from %s\n' "${manifest}" >&2
  exit 1
fi

actual="$("${ctx_bin}" --version)"
expected="ctx ${version}"
if [[ "${actual}" != "${expected}" ]]; then
  printf 'unexpected ctx --version output: got %q, want %q\n' "${actual}" "${expected}" >&2
  exit 1
fi
