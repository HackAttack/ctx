#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/assemble-github-release-assets.sh CORE_DIR CORE_AUTHORITY_DIR RUNTIME_DIR SEMANTIC_PROOF_DIR [OUT_DIR]

Combines an independently staged Core GitHub handoff with the five qualified
ONNX Runtime transports. Final assembly requires cryptographic macOS runtime
signing/attestation evidence plus authoritative native semantic execution
receipts for macOS arm64 and untranslated x64. OUT_DIR defaults to
target/github-release-assets. The input directories are never modified and the
output is published once. Source authority comes from scrubbed checkout HEAD
plus the exact Core handoff; caller-provided source commits are rejected.
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

[[ $# -ge 4 && $# -le 5 ]] || {
  usage
  exit 2
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bundle_tool="${repo_root}/scripts/release/release_bundle.py"
signing_evidence_tool="${repo_root}/scripts/macos-release-signing-evidence.py"
attestation_verifier="${repo_root}/scripts/verify-macos-release-attestation.sh"
core_dir="$1"
core_authority_dir="$2"
runtime_dir="$3"
semantic_proof_dir="$4"
out_dir="${5:-target/github-release-assets}"
for variable in core_dir core_authority_dir runtime_dir semantic_proof_dir out_dir; do
  value="${!variable}"
  [[ "${value}" != -* ]] || die "release directory cannot start with '-': ${value}"
  if [[ "${value}" != /* ]]; then
    printf -v "${variable}" '%s/%s' "${repo_root}" "${value}"
  fi
done

python3 -I "${bundle_tool}" require-directory --directory "${core_dir}"
python3 -I "${bundle_tool}" require-directory --directory "${core_authority_dir}"
python3 -I "${bundle_tool}" require-directory --directory "${runtime_dir}"
python3 -I "${bundle_tool}" require-directory --directory "${semantic_proof_dir}"
python3 -I "${bundle_tool}" preflight-publication \
  --input-dir "${core_dir}" --output-dir "${out_dir}"
python3 -I "${bundle_tool}" preflight-publication \
  --input-dir "${core_authority_dir}" --output-dir "${out_dir}"
python3 -I "${bundle_tool}" preflight-publication \
  --input-dir "${runtime_dir}" --output-dir "${out_dir}"

core_assets=(
  ctx-linux-x64
  ctx-linux-x64.cdx.json
  ctx-linux-x64.third-party-notices.txt
  ctx-linux-aarch64
  ctx-linux-aarch64.cdx.json
  ctx-linux-aarch64.third-party-notices.txt
  ctx-macos-arm64
  ctx-macos-arm64.cdx.json
  ctx-macos-arm64.third-party-notices.txt
  ctx-macos-x64
  ctx-macos-x64.cdx.json
  ctx-macos-x64.third-party-notices.txt
  ctx-windows-x64.exe
  ctx-windows-x64.exe.cdx.json
  ctx-windows-x64.exe.third-party-notices.txt
)
runtime_assets=(
  ctx-onnxruntime-linux-x64.tar.gz
  ctx-onnxruntime-linux-aarch64.tar.gz
  ctx-onnxruntime-macos-arm64.tar.gz
  ctx-onnxruntime-macos-x64.tar.gz
  ctx-onnxruntime-windows-x64.zip
)
release_assets=(
  ctx-linux-aarch64
  ctx-linux-aarch64.cdx.json
  ctx-linux-aarch64.third-party-notices.txt
  ctx-linux-x64
  ctx-linux-x64.cdx.json
  ctx-linux-x64.third-party-notices.txt
  ctx-macos-arm64
  ctx-macos-arm64.cdx.json
  ctx-macos-arm64.third-party-notices.txt
  ctx-macos-x64
  ctx-macos-x64.cdx.json
  ctx-macos-x64.third-party-notices.txt
  ctx-onnxruntime-linux-aarch64.tar.gz
  ctx-onnxruntime-linux-x64.tar.gz
  ctx-onnxruntime-macos-arm64.tar.gz
  ctx-onnxruntime-macos-x64.tar.gz
  ctx-onnxruntime-windows-x64.zip
  ctx-windows-x64.exe
  ctx-windows-x64.exe.cdx.json
  ctx-windows-x64.exe.third-party-notices.txt
)

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{ print $1 }'
  else
    die "sha256sum or shasum is required"
  fi
}

require_regular() {
  [[ -f "$1" && ! -L "$1" ]] || die "$2 must be a regular non-symlink file: $1"
}

[[ -z "${CTX_PUBLIC_RELEASE_SOURCE_COMMIT:-}" ]] \
  || die "caller-provided CTX_PUBLIC_RELEASE_SOURCE_COMMIT is not accepted by final assembly"
git_environment=(
  GIT_ALTERNATE_OBJECT_DIRECTORIES
  GIT_CEILING_DIRECTORIES
  GIT_COMMON_DIR
  GIT_CONFIG_COUNT
  GIT_CONFIG_GLOBAL
  GIT_CONFIG_NOSYSTEM
  GIT_CONFIG_PARAMETERS
  GIT_CONFIG_SYSTEM
  GIT_DIR
  GIT_DISCOVERY_ACROSS_FILESYSTEM
  GIT_EXEC_PATH
  GIT_GRAFT_FILE
  GIT_INDEX_FILE
  GIT_NAMESPACE
  GIT_OBJECT_DIRECTORY
  GIT_REPLACE_REF_BASE
  GIT_SHALLOW_FILE
  GIT_WORK_TREE
)
for git_variable in "${git_environment[@]}"; do
  [[ -z "${!git_variable+x}" ]] \
    || die "hostile Git repository override is not accepted: ${git_variable}"
done
git_scrub=()
for git_variable in "${git_environment[@]}"; do
  git_scrub+=( -u "${git_variable}" )
done
source_commit="$(
  env "${git_scrub[@]}" git -C "${repo_root}" rev-parse --verify HEAD^{commit}
)" || die "could not resolve scrubbed final assembly checkout HEAD"
[[ "${source_commit}" =~ ^[0-9a-f]{40}$ && ! "${source_commit}" =~ ^0{40}$ ]] \
  || die "final assembly checkout source commit is invalid"
buildkite_build_id="${BUILDKITE_BUILD_ID:-}"
buildkite_build_number="${BUILDKITE_BUILD_NUMBER:-}"
[[ "${buildkite_build_id}" =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ \
  && "${buildkite_build_number}" =~ ^[1-9][0-9]*$ ]] \
  || die "final assembly requires immutable Buildkite build identity"

staged=""
validation_work="$(mktemp -d "${TMPDIR:-/tmp}/ctx-github-release-validation.XXXXXX")"
cleanup() {
  if [[ -n "${staged:-}" && -d "${staged}" && ! -L "${staged}" ]]; then
    rm -rf -- "${staged}"
  fi
  if [[ -n "${validation_work:-}" && -d "${validation_work}" \
    && ! -L "${validation_work}" ]]; then
    rm -rf -- "${validation_work}"
  fi
}
trap cleanup EXIT

python3 -I - "${core_dir}" "${core_authority_dir}" "${source_commit}" <<'PY'
import hashlib
import json
import stat
import sys
from pathlib import Path

core_root = Path(sys.argv[1])
root = Path(sys.argv[2])
source_commit = sys.argv[3]
candidates = (
    "ctx-linux-aarch64.candidate.json",
    "ctx.candidate.json",
    "ctx-macos-arm64.candidate.json",
    "ctx-macos-x64.candidate.json",
    "ctx.exe.candidate.json",
)
expected = {
    *(name for candidate in candidates for name in (candidate, f"{candidate}.sha256")),
    "SHA256SUMS",
    "ctx-core-github-handoff.json",
    "ctx-core-github-handoff.json.sha256",
    "ctx-core.release-complete.json",
    "ctx.exe",
    "ctx.exe.build-info.json",
    "ctx.exe.cdx.json",
    "ctx.exe.size.json",
    "ctx.exe.third-party-notices.txt",
    "ctx-release-factory.json",
}
def regular_bytes(path: Path, label: str, maximum: int = 512 * 1024 * 1024) -> bytes:
    try:
        metadata = path.lstat()
    except OSError as error:
        raise SystemExit(f"{label} is unavailable: {path}") from error
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
        raise SystemExit(f"{label} must be a regular non-symlink file")
    if metadata.st_size <= 0 or metadata.st_size > maximum:
        raise SystemExit(f"{label} has an invalid size")
    return path.read_bytes()


actual = {entry.name for entry in root.iterdir()}
if actual != expected:
    raise SystemExit("Core authority handoff inventory is not exact")
for name in expected:
    regular_bytes(root / name, f"Core authority leaf {name}")

handoff_raw = regular_bytes(root / "ctx-core-github-handoff.json", "Core authority handoff", 64 * 1024)
handoff_sha = hashlib.sha256(handoff_raw).hexdigest()
handoff_sum = regular_bytes(
    root / "ctx-core-github-handoff.json.sha256", "Core authority handoff checksum", 128
)
if handoff_sum != f"{handoff_sha}\n".encode("ascii"):
    raise SystemExit("Core authority handoff checksum mismatch")
try:
    handoff = json.loads(handoff_raw)
except (UnicodeDecodeError, json.JSONDecodeError) as error:
    raise SystemExit("Core authority handoff is malformed") from error
if (
    not isinstance(handoff, dict)
    or set(handoff) != {
        "candidate_manifests",
        "factory_completion",
        "factory_manifest",
        "kind",
        "release_sums",
        "schema_version",
        "source_commit",
    }
    or handoff.get("kind") != "ctx-public-core-github-handoff"
    or handoff.get("schema_version") != 1
    or handoff.get("source_commit") != source_commit
):
    raise SystemExit("Core authority handoff does not bind scrubbed checkout HEAD")


def verify_record(record: object, name: str, label: str) -> None:
    payload = regular_bytes(root / name, label)
    expected_record = {
        "file": name,
        "sha256": hashlib.sha256(payload).hexdigest(),
        "size_bytes": len(payload),
    }
    if record != expected_record:
        raise SystemExit(f"Core authority handoff has a mismatched {label} record")


records = handoff.get("candidate_manifests")
if not isinstance(records, list) or len(records) != len(candidates):
    raise SystemExit("Core authority handoff candidate manifest inventory is invalid")
for record, name in zip(records, candidates, strict=True):
    verify_record(record, name, name)
    payload = regular_bytes(root / name, name)
    sidecar = regular_bytes(root / f"{name}.sha256", f"{name} checksum", 128)
    if sidecar != f"{hashlib.sha256(payload).hexdigest()}\n".encode("ascii"):
        raise SystemExit(f"Core authority candidate checksum mismatch: {name}")
verify_record(
    handoff.get("factory_completion"),
    "ctx-core.release-complete.json",
    "factory completion",
)
verify_record(
    handoff.get("factory_manifest"),
    "ctx-release-factory.json",
    "factory manifest",
)
verify_record(handoff.get("release_sums"), "SHA256SUMS", "release checksum manifest")
authority_sums = regular_bytes(root / "SHA256SUMS", "authority release checksum manifest", 4096)
core_sums = regular_bytes(core_root / "SHA256SUMS", "Core release checksum manifest", 4096)
if authority_sums != core_sums:
    raise SystemExit("Core assets and source-bound authority use different SHA256SUMS")
PY

declare -A core_digests=()
core_names=()
core_sums="${core_dir%/}/SHA256SUMS"
require_regular "${core_sums}" "Core checksum manifest"
while IFS= read -r line; do
  [[ "${line}" =~ ^([0-9a-f]{64})\ \ ([A-Za-z0-9][A-Za-z0-9._-]{0,127})$ ]] \
    || die "Core SHA256SUMS is malformed"
  digest="${BASH_REMATCH[1]}"
  name="${BASH_REMATCH[2]}"
  [[ ! -v "core_digests[${name}]" ]] || die "Core SHA256SUMS repeats ${name}"
  core_digests["${name}"]="${digest}"
  core_names+=("${name}")
done < "${core_sums}"
[[ "${#core_names[@]}" -eq "${#core_assets[@]}" ]] \
  || die "Core SHA256SUMS must contain exactly 15 assets"
for asset in "${core_assets[@]}"; do
  source_path="${core_dir%/}/${asset}"
  require_regular "${source_path}" "Core release asset"
  [[ -v "core_digests[${asset}]" ]] || die "Core SHA256SUMS is missing ${asset}"
  actual="$(sha256_file "${source_path}")"
  [[ "${actual}" == "${core_digests[${asset}]}" ]] \
    || die "Core checksum mismatch for ${asset}"
done

declare -A runtime_digests=()
for asset in "${runtime_assets[@]}"; do
  source_path="${runtime_dir%/}/${asset}"
  checksum_path="${source_path}.sha256"
  require_regular "${source_path}" "runtime release asset"
  require_regular "${checksum_path}" "runtime checksum"
  IFS= read -r expected < "${checksum_path}" || true
  [[ "${expected}" =~ ^[0-9a-f]{64}$ ]] \
    || die "runtime checksum is malformed for ${asset}"
  [[ "$(wc -l < "${checksum_path}" | tr -d '[:space:]')" == "1" ]] \
    || die "runtime checksum must contain one line for ${asset}"
  actual="$(sha256_file "${source_path}")"
  [[ "${actual}" == "${expected}" ]] || die "runtime checksum mismatch for ${asset}"
  runtime_digests["${asset}"]="${actual}"
done

declare -A macos_nested_artifacts=()
for platform in macos-arm64 macos-x64; do
  prefix="ctx-onnxruntime-${platform}"
  archive="${runtime_dir%/}/${prefix}.tar.gz"
  checksum="${archive}.sha256"
  signing_evidence="${runtime_dir%/}/${prefix}.signing.json"
  notary_submit="${runtime_dir%/}/${prefix}.notary-submit.json"
  artifact_attestation="${runtime_dir%/}/${prefix}.attestation.json"
  artifact_attestation_cms="${runtime_dir%/}/${prefix}.attestation.cms"
  release_attestation="${runtime_dir%/}/${prefix}.release-attestation.json"
  release_attestation_cms="${runtime_dir%/}/${prefix}.release-attestation.cms"
  for evidence_path in \
    "${signing_evidence}" \
    "${notary_submit}" \
    "${artifact_attestation}" \
    "${artifact_attestation_cms}" \
    "${release_attestation}" \
    "${release_attestation_cms}"; do
    require_regular "${evidence_path}" "macOS runtime release evidence"
    [[ -s "${evidence_path}" ]] || die "macOS runtime release evidence is empty: ${evidence_path}"
  done

  nested_artifact="${validation_work}/${platform}/libonnxruntime.dylib"
  mkdir -p "$(dirname "${nested_artifact}")"
  python3 -I - "${archive}" "${nested_artifact}" <<'PY'
import shutil
import stat
import sys
import tarfile
from pathlib import PurePosixPath

archive, output = sys.argv[1:]
expected_files = {
    "GIT_COMMIT_ID",
    "LICENSE",
    "ThirdPartyNotices.txt",
    "VERSION_NUMBER",
    "lib/libonnxruntime.dylib",
}
expected_entries = expected_files | {"lib"}
members = {}
with tarfile.open(archive, "r:gz") as bundle:
    for member in bundle.getmembers():
        name = member.name[:-1] if member.name.endswith("/") else member.name
        path = PurePosixPath(name)
        if (
            not name
            or path.is_absolute()
            or str(path) != name
            or any(part in {"", ".", ".."} for part in path.parts)
            or name in members
            or name not in expected_entries
            or member.mode & 0o7000
        ):
            raise SystemExit(f"unsafe or unexpected macOS runtime archive entry: {member.name!r}")
        if name == "lib":
            if not member.isdir():
                raise SystemExit("macOS runtime lib entry is not a directory")
        elif not member.isfile() or stat.S_ISLNK(member.mode):
            raise SystemExit(f"macOS runtime entry is not a regular file: {name}")
        members[name] = member
    if set(members) != expected_entries:
        raise SystemExit("macOS runtime archive does not have the exact release layout")
    source = bundle.extractfile(members["lib/libonnxruntime.dylib"])
    if source is None:
        raise SystemExit("could not read final lib/libonnxruntime.dylib")
    with source, open(output, "xb") as destination:
        shutil.copyfileobj(source, destination)
PY
  [[ -s "${nested_artifact}" ]] || die "final macOS runtime dylib is empty"
  macos_nested_artifacts["${platform}"]="${nested_artifact}"

  python3 -I "${signing_evidence_tool}" verify-archive \
    --evidence "${signing_evidence}" \
    --platform "${platform}" \
    --archive "${archive}" \
    --checksum "${checksum}" \
    --nested-artifact "${nested_artifact}" \
    --role release
  python3 -I - \
    "${signing_evidence}" "${notary_submit}" \
    "${artifact_attestation}" "${release_attestation}" <<'PY'
import hashlib
import json
import sys

signing_path, notary_path, artifact_path, release_path = sys.argv[1:]
with open(signing_path, encoding="utf-8") as source:
    signing = json.load(source)
with open(artifact_path, encoding="utf-8") as source:
    artifact = json.load(source)
with open(release_path, encoding="utf-8") as source:
    release = json.load(source)
with open(notary_path, "rb") as source:
    notary_sha256 = hashlib.sha256(source.read()).hexdigest()

codesign = signing.get("codesign") if isinstance(signing, dict) else None
notarization = signing.get("notarization") if isinstance(signing, dict) else None
identity = (
    codesign.get("authority") if isinstance(codesign, dict) else None,
    codesign.get("team_identifier") if isinstance(codesign, dict) else None,
)
if (
    identity != (artifact.get("codesign_authority"), artifact.get("team_identifier"))
    or identity != (release.get("codesign_authority"), release.get("team_identifier"))
):
    raise SystemExit(
        "macOS runtime signing evidence identity does not match its cryptographic attestations"
    )
if not isinstance(notarization, dict) or notarization.get("submit_sha256") != notary_sha256:
    raise SystemExit("macOS runtime signing evidence does not bind the notarization response")
PY
  CTX_MACOS_RELEASE_SOURCE_COMMIT="${source_commit}" \
    "${attestation_verifier}" \
      "${platform}" runtime "${nested_artifact}" \
      "${artifact_attestation}" "${artifact_attestation_cms}" >/dev/null
  CTX_MACOS_RELEASE_SOURCE_COMMIT="${source_commit}" \
    "${attestation_verifier}" --runtime-archive \
      "${platform}" "${archive}" "${nested_artifact}" \
      "${release_attestation}" "${release_attestation_cms}" >/dev/null
done

python3 -I - \
  "${core_dir}" "${runtime_dir}" "${semantic_proof_dir}" \
  "${macos_nested_artifacts[macos-arm64]}" \
  "${macos_nested_artifacts[macos-x64]}" \
  "${source_commit}" "${buildkite_build_id}" "${buildkite_build_number}" <<'PY'
import hashlib
import json
import re
import stat
import sys
from pathlib import Path

core_root = Path(sys.argv[1])
runtime_root = Path(sys.argv[2])
proof_root = Path(sys.argv[3])
nested = {
    "macos-arm64": Path(sys.argv[4]),
    "macos-x64": Path(sys.argv[5]),
}
source_commit = sys.argv[6]
build_id = sys.argv[7]
build_number = int(sys.argv[8])
uuid = re.compile(
    r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}"
)
producer_steps = {
    "macos-arm64": "github-macos-arm64-semantic-native-smoke",
    "macos-x64": "github-macos-x64-semantic-native-smoke",
}


def digest(path: Path, label: str, maximum: int) -> str:
    try:
        metadata = path.lstat()
    except OSError as error:
        raise SystemExit(f"{label} is unavailable: {path}") from error
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
        raise SystemExit(f"{label} must be a regular non-symlink file")
    if metadata.st_size <= 0 or metadata.st_size > maximum:
        raise SystemExit(f"{label} has an invalid size")
    value = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            value.update(chunk)
    return value.hexdigest()


def require_exact_host(platform: str, host: object) -> None:
    if not isinstance(host, dict) or set(host) != {
        "arch",
        "emulation",
        "evidence_complete",
        "hardware_identity",
        "hypervisor",
        "native_arch",
        "native_arch_probe",
        "process_translated",
        "runner_id",
        "system",
    }:
        raise SystemExit(f"native semantic receipt has invalid {platform} host evidence")
    if (
        host["system"] != "Darwin"
        or host["native_arch_probe"] != "sysctl"
        or host["process_translated"] != 0
        or host["evidence_complete"] is not True
    ):
        raise SystemExit(f"native semantic receipt is not untranslated complete {platform} evidence")
    if platform == "macos-arm64":
        valid = (
            host["arch"] == "arm64"
            and host["native_arch"] == "arm64"
            and host["hardware_identity"] == "apple"
            and host["emulation"] == "none"
            and host["hypervisor"] == "absent"
            and host["runner_id"] == ""
        )
    else:
        physical = (
            host["hardware_identity"] == "apple"
            and host["emulation"] == "none"
            and host["hypervisor"] == "absent"
            and host["runner_id"] == ""
        )
        pinned_untranslated = (
            host["hardware_identity"] == "generic"
            and host["emulation"] == "qemu-kvm"
            and host["hypervisor"] == "present"
            and host["runner_id"] == "ctx-mac-gui-shared-x64"
        )
        valid = (
            host["arch"] == "x86_64"
            and host["native_arch"] == "x86_64"
            and (physical or pinned_untranslated)
        )
    if not valid:
        raise SystemExit(f"native semantic receipt has non-authoritative {platform} host evidence")


for platform in ("macos-arm64", "macos-x64"):
    proof = proof_root / platform / f"ctx-{platform}.semantic-execution.json"
    proof_sha256 = digest(proof, f"{platform} native semantic receipt", 64 * 1024)
    artifact_path = (
        f"target/public-cli-semantic-native-smoke/{platform}/"
        f"ctx-{platform}.semantic-execution.json"
    )
    authority_path = proof.with_name(
        f"ctx-{platform}.semantic-execution.artifact-authority.json"
    )
    digest(authority_path, f"{platform} semantic receipt artifact authority", 64 * 1024)
    try:
        value = json.loads(proof.read_text(encoding="utf-8"))
        artifact_authority = json.loads(authority_path.read_text(encoding="utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise SystemExit(f"native semantic receipt authority is malformed for {platform}") from error
    expected_step = producer_steps[platform]
    if (
        not isinstance(artifact_authority, dict)
        or artifact_authority
        != {
            "artifact_path": artifact_path,
            "artifact_sha256": proof_sha256,
            "attempt_selection": "latest",
            "build_id": build_id,
            "build_number": build_number,
            "kind": "ctx-buildkite-semantic-receipt-artifact-authority",
            "producer_job_id": artifact_authority.get("producer_job_id"),
            "producer_step_key": expected_step,
            "schema_version": 1,
        }
        or uuid.fullmatch(str(artifact_authority.get("producer_job_id"))) is None
    ):
        raise SystemExit(
            f"{platform} semantic receipt lacks exact Buildkite artifact authority"
        )
    provenance = value.get("provenance") if isinstance(value, dict) else None
    if (
        not isinstance(provenance, dict)
        or set(provenance) != {
            "build_id",
            "build_number",
            "job_id",
            "retry_count",
            "source_commit",
            "step_key",
        }
        or provenance.get("build_id") != build_id
        or provenance.get("build_number") != build_number
        or provenance.get("source_commit") != source_commit
        or provenance.get("step_key") != expected_step
        or provenance.get("job_id") != artifact_authority.get("producer_job_id")
        or uuid.fullmatch(str(provenance.get("job_id"))) is None
        or isinstance(provenance.get("retry_count"), bool)
        or not isinstance(provenance.get("retry_count"), int)
        or provenance["retry_count"] < 0
    ):
        raise SystemExit(
            f"{platform} semantic receipt provenance is replayed or forged"
        )
    cli_name = f"ctx-{platform}"
    runtime_name = f"ctx-onnxruntime-{platform}.tar.gz"
    expected_cli = {"name": cli_name, "sha256": digest(core_root / cli_name, "final CLI", 256 * 1024 * 1024)}
    expected_runtime = {
        "name": runtime_name,
        "nested_artifact_name": "lib/libonnxruntime.dylib",
        "nested_artifact_sha256": digest(nested[platform], "final nested runtime", 256 * 1024 * 1024),
        "sha256": digest(runtime_root / runtime_name, "final runtime archive", 1024 * 1024 * 1024),
    }
    expected_semantic = {
        "effective_mode": "semantic",
        "indexed_chunks_minimum": 1,
        "model_key": "e5-small-v1:mean-pool:l2:query-passage",
        "requested_mode": "semantic",
        "status": "ready",
    }
    if (
        not isinstance(value, dict)
        or set(value) != {
            "authority",
            "backend",
            "cli_artifact",
            "host_evidence",
            "kind",
            "platform",
            "provenance",
            "runtime_archive",
            "schema_version",
            "semantic",
            "status",
        }
        or value.get("schema_version") != 2
        or value.get("kind") != "ctx-semantic-native-execution"
        or value.get("platform") != platform
        or value.get("status") != "passed"
        or value.get("authority") != "authoritative"
        or value.get("backend") != "onnxruntime-cpu"
        or value.get("cli_artifact") != expected_cli
        or value.get("runtime_archive") != expected_runtime
        or value.get("semantic") != expected_semantic
    ):
        raise SystemExit(f"native semantic receipt does not bind the exact final {platform} CLI/runtime bytes")
    require_exact_host(platform, value.get("host_evidence"))
PY

staged="$(mktemp -d "$(dirname "${out_dir}")/.github-release-final.XXXXXX")"

for asset in "${core_assets[@]}"; do
  mode=0644
  case "${asset}" in
    ctx-linux-x64|ctx-linux-aarch64|ctx-macos-arm64|ctx-macos-x64)
      mode=0755
      ;;
  esac
  install -m "${mode}" "${core_dir%/}/${asset}" "${staged}/${asset}"
  [[ "$(sha256_file "${staged}/${asset}")" == "${core_digests[${asset}]}" ]] \
    || die "Core asset changed while staged: ${asset}"
done
for asset in "${runtime_assets[@]}"; do
  install -m 0644 "${runtime_dir%/}/${asset}" "${staged}/${asset}"
  [[ "$(sha256_file "${staged}/${asset}")" == "${runtime_digests[${asset}]}" ]] \
    || die "runtime asset changed while staged: ${asset}"
done

for asset in "${release_assets[@]}"; do
  printf '%s  %s\n' "$(sha256_file "${staged}/${asset}")" "${asset}" \
    >> "${staged}/SHA256SUMS"
done
[[ "$(find "${staged}" -maxdepth 1 -type f | wc -l)" == "21" ]] \
  || die "final GitHub release inventory is not exactly 21 files"

python3 -I "${bundle_tool}" commit-directory \
  --stage-dir "${staged}" --output-dir "${out_dir}"
rm -rf -- "${validation_work}"
validation_work=""
trap - EXIT
printf 'assembled GitHub release assets in %s\n' "${out_dir}"
