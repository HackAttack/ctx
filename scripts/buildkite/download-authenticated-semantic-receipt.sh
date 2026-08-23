#!/usr/bin/env bash
set -euo pipefail
umask 077

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/buildkite/download-authenticated-semantic-receipt.sh PLATFORM

Downloads the latest-attempt semantic receipt from its exact producer job in
the current Buildkite build, verifies the Buildkite API SHA-256, and writes a
local artifact-authority record for final release assembly.
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

[[ $# -eq 1 ]] || {
  usage
  exit 2
}

platform="$1"
case "${platform}" in
  macos-arm64)
    producer_step="github-macos-arm64-semantic-native-smoke"
    ;;
  macos-x64)
    producer_step="github-macos-x64-semantic-native-smoke"
    ;;
  *)
    usage
    exit 2
    ;;
esac

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
proof_root="${repo_root}/target/public-cli-semantic-native-smoke"

build_id="${BUILDKITE_BUILD_ID:-}"
build_number="${BUILDKITE_BUILD_NUMBER:-}"
uuid_pattern='^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
[[ "${build_id}" =~ ${uuid_pattern} && "${build_number}" =~ ^[1-9][0-9]*$ ]] \
  || die "authenticated receipt download requires immutable Buildkite build identity"
command -v buildkite-agent >/dev/null 2>&1 \
  || die "buildkite-agent is required for authenticated receipt download"

artifact_path="target/public-cli-semantic-native-smoke/${platform}/ctx-${platform}.semantic-execution.json"
local_receipt="${proof_root%/}/${platform}/ctx-${platform}.semantic-execution.json"
authority="${proof_root%/}/${platform}/ctx-${platform}.semantic-execution.artifact-authority.json"
mkdir -p "$(dirname "${local_receipt}")"
for output in "${local_receipt}" "${authority}"; do
  [[ ! -e "${output}" && ! -L "${output}" ]] \
    || die "authenticated receipt output already exists: ${output}"
done

mapfile -t matches < <(
  BUILDKITE_AGENT_INCLUDE_RETRIED_JOBS=false \
    buildkite-agent artifact search "${artifact_path}" \
    --step "${producer_step}" \
    --build "${build_id}" \
    --format '%j\t%p\t%T\n'
)
[[ "${#matches[@]}" == "1" && -n "${matches[0]}" ]] \
  || die "expected exactly one latest-attempt ${platform} semantic receipt artifact"
IFS=$'\t' read -r producer_job_id matched_path artifact_sha256 extra <<<"${matches[0]}"
[[ -z "${extra:-}" \
  && "${producer_job_id}" =~ ${uuid_pattern} \
  && "${matched_path}" == "${artifact_path}" \
  && "${artifact_sha256}" =~ ^[0-9a-f]{64}$ ]] \
  || die "Buildkite returned malformed semantic receipt artifact authority"

(
  cd "${repo_root}"
  BUILDKITE_AGENT_INCLUDE_RETRIED_JOBS=false \
    buildkite-agent artifact download "${artifact_path}" . \
    --step "${producer_job_id}" \
    --build "${build_id}"
)
[[ -f "${local_receipt}" && ! -L "${local_receipt}" && -s "${local_receipt}" ]] \
  || die "authenticated semantic receipt download did not produce a regular file"
if command -v sha256sum >/dev/null 2>&1; then
  local_sha256="$(sha256sum "${local_receipt}" | awk '{print $1}')"
elif command -v shasum >/dev/null 2>&1; then
  local_sha256="$(shasum -a 256 "${local_receipt}" | awk '{print $1}')"
else
  die "sha256sum or shasum is required"
fi
[[ "${local_sha256}" == "${artifact_sha256}" ]] \
  || die "downloaded semantic receipt does not match Buildkite artifact SHA-256"

python3 -I - \
  "${authority}" "${artifact_path}" "${artifact_sha256}" \
  "${build_id}" "${build_number}" "${producer_job_id}" "${producer_step}" <<'PY'
import json
import os
import sys
from pathlib import Path

output_text, artifact_path, artifact_sha256, build_id, build_number, job_id, step_key = sys.argv[1:]
document = {
    "artifact_path": artifact_path,
    "artifact_sha256": artifact_sha256,
    "attempt_selection": "latest",
    "build_id": build_id,
    "build_number": int(build_number),
    "kind": "ctx-buildkite-semantic-receipt-artifact-authority",
    "producer_job_id": job_id,
    "producer_step_key": step_key,
    "schema_version": 1,
}
output = Path(output_text)
temporary = output.with_name(f".{output.name}.tmp.{os.getpid()}")
descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
try:
    with os.fdopen(descriptor, "w", encoding="utf-8", newline="\n") as stream:
        json.dump(document, stream, sort_keys=True, separators=(",", ":"))
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    if output.exists() or output.is_symlink():
        raise SystemExit(f"artifact authority already exists: {output}")
    os.replace(temporary, output)
finally:
    temporary.unlink(missing_ok=True)
PY

printf 'authenticated Buildkite semantic receipt: %s job=%s\n' \
  "${platform}" "${producer_job_id}"
