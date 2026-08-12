#!/usr/bin/env bash
set -euo pipefail

[[ $# -eq 3 ]] || {
  printf 'usage: %s PLATFORM ARCHIVE NESTED_DYLIB\n' "$0" >&2
  exit 2
}
platform="$1"
archive="$2"
nested_artifact="$3"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
evidence="$(dirname "${archive}")/ctx-onnxruntime-${platform}.signing.json"

if [[ "$(uname -s)" == "Darwin" ]]; then
  exec "${script_dir}/check-macos-release-signing.sh" \
    "${platform}" runtime "${archive}" "${evidence}"
fi
python3 "${script_dir}/macos-release-signing-evidence.py" verify-archive \
  --evidence "${evidence}" --platform "${platform}" --archive "${archive}" \
  --checksum "${archive}.sha256" --nested-artifact "${nested_artifact}" \
  --role builder
CTX_MACOS_RELEASE_SOURCE_COMMIT="$(git -C "${script_dir}/.." rev-parse --verify HEAD)" \
  "${script_dir}/verify-macos-release-attestation.sh" \
  "${platform}" runtime "${nested_artifact}" \
  "$(dirname "${archive}")/ctx-onnxruntime-${platform}.attestation.json" \
  "$(dirname "${archive}")/ctx-onnxruntime-${platform}.attestation.cms"
