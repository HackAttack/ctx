#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${TEST_SRCDIR:-}" && -n "${TEST_WORKSPACE:-}" ]]; then
  source_root="${TEST_SRCDIR}/${TEST_WORKSPACE}"
else
  source_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fi
tmp="$(mktemp -d "${TMPDIR:-/tmp}/ctx-assemble-github-assets.XXXXXX")"
trap 'rm -rf "${tmp}"' EXIT
test_repo="${tmp}/repo"
mkdir -p "${test_repo}/scripts/release"
for source in \
  scripts/apple-developer-id-g2-ca.pem \
  scripts/assemble-github-release-assets.sh \
  scripts/macos-release-publisher-policy.sh \
  scripts/macos-release-signing-evidence.py \
  scripts/release/release_bundle.py \
  scripts/verify-macos-release-attestation.sh; do
  mkdir -p "${test_repo}/$(dirname "${source}")"
  cp -L "${source_root}/${source}" "${test_repo}/${source}"
done
chmod 0755 \
  "${test_repo}/scripts/assemble-github-release-assets.sh" \
  "${test_repo}/scripts/macos-release-signing-evidence.py" \
  "${test_repo}/scripts/verify-macos-release-attestation.sh"
git -C "${test_repo}" init -q
git -C "${test_repo}" config user.name 'ctx release assembly test'
git -C "${test_repo}" config user.email 'ctx-release-assembly@example.invalid'
git -C "${test_repo}" add .
git -C "${test_repo}" commit -qm 'seal assembly fixture checkout'
source_commit="$(git -C "${test_repo}" rev-parse --verify HEAD^{commit})"
assembler="${test_repo}/scripts/assemble-github-release-assets.sh"
evidence_tool="${test_repo}/scripts/macos-release-signing-evidence.py"
core="${tmp}/core"
core_authority="${tmp}/core-authority"
runtime="${tmp}/runtime"
proof="${tmp}/semantic-proof"
fake_bin="${tmp}/fake-bin"
mkdir -p "${core}" "${core_authority}" "${runtime}" "${proof}" "${fake_bin}"
buildkite_build_id='11111111-1111-4111-8111-111111111111'
buildkite_build_number=42

cat > "${fake_bin}/openssl" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
command_name="${1:-}"
shift || true
case "${command_name}" in
  version)
    printf 'OpenSSL 3.3.0 ctx test fixture\n'
    ;;
  cms)
    if [[ "${1:-}" == "-help" ]]; then
      printf '%s\n' '-no-CApath -no-CAstore -ignore_critical'
      exit 0
    fi
    input=""
    signer=""
    while (($# > 0)); do
      case "$1" in
        -in) input="$2"; shift 2 ;;
        -signer) signer="$2"; shift 2 ;;
        *) shift ;;
      esac
    done
    [[ -n "${input}" && -n "${signer}" ]]
    [[ "$(cat "${input}")" == "valid-cms" ]] || exit 1
    printf '%s\n' \
      '-----BEGIN CERTIFICATE-----' \
      'fake signer certificate' \
      '-----END CERTIFICATE-----' > "${signer}"
    ;;
  x509)
    if [[ " $* " == *' -fingerprint '* ]]; then
      printf '%s\n' 'sha256 Fingerprint=F1:6C:D3:C5:4C:7F:83:CE:A4:BF:1A:3E:6A:08:19:C8:AA:A8:E4:A1:52:8F:D1:44:71:5F:35:06:43:D2:DF:3A'
    elif [[ " $* " == *' -subject '* ]]; then
      team="${CTX_TEST_FAKE_SIGNER_TEAM_ID:-TESTTEAM01}"
      if [[ "${team}" == "TESTTEAM01" ]]; then
        name='Fixture Publisher'
      else
        name='Other Publisher'
      fi
      printf 'subject=OU=%s,CN=Developer ID Application: %s (%s)\n' \
        "${team}" "${name}" "${team}"
    elif [[ " $* " == *' -ext extendedKeyUsage '* ]]; then
      printf '%s\n' 'X509v3 Extended Key Usage:' '    Code Signing'
    elif [[ " $* " == *' -ext keyUsage '* ]]; then
      printf '%s\n' 'X509v3 Key Usage: critical' '    Digital Signature'
    elif [[ " $* " == *' -text '* ]]; then
      printf '%s\n' '1.2.840.113635.100.6.1.13: critical'
    else
      exit 1
    fi
    ;;
  verify)
    exit 0
    ;;
  dgst)
    team_id="$(cat)"
    if [[ "${team_id}" == "TESTTEAM01" ]]; then
      printf '%s *stdin\n' '013a2701d0f3400afe5257f41fce0e2d4276ef37981e443b1d3aeb442a95763c'
    else
      printf '%s' "${team_id}" | sha256sum | awk '{print $1 " *stdin"}'
    fi
    ;;
  *)
    exit 1
    ;;
esac
SH
chmod 0755 "${fake_bin}/openssl"
export PATH="${fake_bin}:${PATH}"

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

for asset in "${core_assets[@]}"; do
  printf 'qualified Core fixture: %s\n' "${asset}" > "${core}/${asset}"
  printf '%s  %s\n' "$(sha256sum "${core}/${asset}" | awk '{print $1}')" "${asset}" \
    >> "${core}/SHA256SUMS"
done
cp "${core}/SHA256SUMS" "${core_authority}/SHA256SUMS"
for candidate in \
  ctx-linux-aarch64.candidate.json \
  ctx.candidate.json \
  ctx-macos-arm64.candidate.json \
  ctx-macos-x64.candidate.json \
  ctx.exe.candidate.json; do
  printf '{"fixture":"%s"}\n' "${candidate}" > "${core_authority}/${candidate}"
  sha256sum "${core_authority}/${candidate}" | awk '{print $1}' \
    > "${core_authority}/${candidate}.sha256"
done
for authority_leaf in \
  ctx-core.release-complete.json \
  ctx.exe \
  ctx.exe.build-info.json \
  ctx.exe.cdx.json \
  ctx.exe.size.json \
  ctx.exe.third-party-notices.txt \
  ctx-release-factory.json; do
  printf 'Core authority fixture: %s\n' "${authority_leaf}" \
    > "${core_authority}/${authority_leaf}"
done
python3 - "${core_authority}" "${source_commit}" <<'PY'
import hashlib
import json
import sys
from pathlib import Path

root = Path(sys.argv[1])
source_commit = sys.argv[2]
candidates = (
    "ctx-linux-aarch64.candidate.json",
    "ctx.candidate.json",
    "ctx-macos-arm64.candidate.json",
    "ctx-macos-x64.candidate.json",
    "ctx.exe.candidate.json",
)


def record(name):
    raw = (root / name).read_bytes()
    return {"file": name, "sha256": hashlib.sha256(raw).hexdigest(), "size_bytes": len(raw)}


document = {
    "candidate_manifests": [record(name) for name in candidates],
    "factory_completion": record("ctx-core.release-complete.json"),
    "factory_manifest": record("ctx-release-factory.json"),
    "kind": "ctx-public-core-github-handoff",
    "release_sums": record("SHA256SUMS"),
    "schema_version": 1,
    "source_commit": source_commit,
}
raw = (json.dumps(document, sort_keys=True, separators=(",", ":")) + "\n").encode()
(root / "ctx-core-github-handoff.json").write_bytes(raw)
(root / "ctx-core-github-handoff.json.sha256").write_text(
    hashlib.sha256(raw).hexdigest() + "\n", encoding="ascii"
)
PY
for asset in "${runtime_assets[@]}"; do
  case "${asset}" in
    ctx-onnxruntime-macos-*) continue ;;
  esac
  printf 'qualified runtime fixture: %s\n' "${asset}" > "${runtime}/${asset}"
  sha256sum "${runtime}/${asset}" | awk '{print $1}' > "${runtime}/${asset}.sha256"
done

authority='Developer ID Application: Fixture Publisher (TESTTEAM01)'
for platform in macos-arm64 macos-x64; do
  prefix="ctx-onnxruntime-${platform}"
  package="${tmp}/package-${platform}"
  mkdir -p "${package}/lib"
  printf 'license\n' > "${package}/LICENSE"
  printf 'notices\n' > "${package}/ThirdPartyNotices.txt"
  printf '1.27.0\n' > "${package}/VERSION_NUMBER"
  printf '0123456789abcdef0123456789abcdef01234567\n' > "${package}/GIT_COMMIT_ID"
  printf 'signed ctx Team runtime fixture: %s\n' "${platform}" \
    > "${package}/lib/libonnxruntime.dylib"
  archive="${runtime}/${prefix}.tar.gz"
  tar --no-recursion -czf "${archive}" -C "${package}" \
    GIT_COMMIT_ID LICENSE ThirdPartyNotices.txt VERSION_NUMBER lib \
    lib/libonnxruntime.dylib
  sha256sum "${archive}" | awk '{print $1}' > "${archive}.sha256"
  notary="${runtime}/${prefix}.notary-submit.json"
  printf '{"id":"fixture-%s","status":"Accepted"}\n' "${platform}" > "${notary}"
  python3 - "${archive}" "${package}/lib/libonnxruntime.dylib" \
    "${notary}" "${runtime}/${prefix}.signing.json" "${platform}" <<'PY'
import hashlib
import json
import sys
from pathlib import Path

archive, nested, notary, output, platform = map(Path, sys.argv[1:])


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


value = {
    "artifact_kind": "runtime",
    "artifact_name": "libonnxruntime.dylib",
    "artifact_sha256": sha256(nested),
    "artifact_verification": {
        "method": "accepted-notary-strict-codesign-attestation",
        "status": "passed",
    },
    "codesign": {
        "authority": "Developer ID Application: Fixture Publisher (TESTTEAM01)",
        "hardened_runtime": True,
        "identifier": "ctx",
        "secure_timestamp": True,
        "team_identifier": "TESTTEAM01",
        "verified": True,
    },
    "notarization": {
        "status": "Accepted",
        "submission_id": f"fixture-{platform}",
        "submit_sha256": sha256(notary),
    },
    "packages": [
        {
            "archive_name": archive.name,
            "archive_sha256": sha256(archive),
            "nested_artifact_sha256": sha256(nested),
            "role": "release",
        }
    ],
    "platform": str(platform),
    "schema_version": 2,
}
output.write_text(json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n")
PY
  python3 "${evidence_tool}" create-attestation \
    --output "${runtime}/${prefix}.attestation.json" \
    --platform "${platform}" --kind runtime \
    --artifact "${package}/lib/libonnxruntime.dylib" \
    --notary-submit "${notary}" --source-commit "${source_commit}" \
    --codesign-authority "${authority}"
  python3 "${evidence_tool}" create-runtime-archive-attestation \
    --output "${runtime}/${prefix}.release-attestation.json" \
    --platform "${platform}" --archive "${archive}" \
    --nested-artifact "${package}/lib/libonnxruntime.dylib" \
    --notary-submit "${notary}" --source-commit "${source_commit}" \
    --codesign-authority "${authority}"
  printf 'valid-cms\n' > "${runtime}/${prefix}.attestation.cms"
  printf 'valid-cms\n' > "${runtime}/${prefix}.release-attestation.cms"

  mkdir -p "${proof}/${platform}"
  python3 - "${core}/ctx-${platform}" "${archive}" \
    "${package}/lib/libonnxruntime.dylib" \
    "${proof}/${platform}/ctx-${platform}.semantic-execution.json" \
    "${platform}" "${source_commit}" "${buildkite_build_id}" \
    "${buildkite_build_number}" <<'PY'
import hashlib
import json
import sys
from pathlib import Path

cli, archive, nested, output, platform = map(Path, sys.argv[1:6])
source_commit, build_id, build_number = sys.argv[6:]


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


if str(platform) == "macos-arm64":
    host = {
        "arch": "arm64",
        "emulation": "none",
        "evidence_complete": True,
        "hardware_identity": "apple",
        "hypervisor": "absent",
        "native_arch": "arm64",
        "native_arch_probe": "sysctl",
        "process_translated": 0,
        "runner_id": "",
        "system": "Darwin",
    }
else:
    host = {
        "arch": "x86_64",
        "emulation": "qemu-kvm",
        "evidence_complete": True,
        "hardware_identity": "generic",
        "hypervisor": "present",
        "native_arch": "x86_64",
        "native_arch_probe": "sysctl",
        "process_translated": 0,
        "runner_id": "ctx-mac-gui-shared-x64",
        "system": "Darwin",
    }
step_key = f"github-{platform}-semantic-native-smoke"
job_id = (
    "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
    if str(platform) == "macos-arm64"
    else "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
)
value = {
    "authority": "authoritative",
    "backend": "onnxruntime-cpu",
    "cli_artifact": {"name": cli.name, "sha256": sha256(cli)},
    "host_evidence": host,
    "kind": "ctx-semantic-native-execution",
    "platform": str(platform),
    "provenance": {
        "build_id": build_id,
        "build_number": int(build_number),
        "job_id": job_id,
        "retry_count": 1,
        "source_commit": source_commit,
        "step_key": step_key,
    },
    "runtime_archive": {
        "name": archive.name,
        "nested_artifact_name": "lib/libonnxruntime.dylib",
        "nested_artifact_sha256": sha256(nested),
        "sha256": sha256(archive),
    },
    "schema_version": 2,
    "semantic": {
        "effective_mode": "semantic",
        "indexed_chunks_minimum": 1,
        "model_key": "e5-small-v1:mean-pool:l2:query-passage",
        "requested_mode": "semantic",
        "status": "ready",
    },
    "status": "passed",
}
output.write_text(json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n")
receipt_sha256 = sha256(output)
authority_output = output.with_name(
    f"ctx-{platform}.semantic-execution.artifact-authority.json"
)
artifact_authority = {
    "artifact_path": (
        f"target/public-cli-semantic-native-smoke/{platform}/"
        f"ctx-{platform}.semantic-execution.json"
    ),
    "artifact_sha256": receipt_sha256,
    "attempt_selection": "latest",
    "build_id": build_id,
    "build_number": int(build_number),
    "kind": "ctx-buildkite-semantic-receipt-artifact-authority",
    "producer_job_id": job_id,
    "producer_step_key": step_key,
    "schema_version": 1,
}
authority_output.write_text(
    json.dumps(artifact_authority, sort_keys=True, separators=(",", ":")) + "\n"
)
PY
done

expect_failure() {
  local name="$1"
  local expected="$2"
  local core_input="$3"
  local core_authority_input="$4"
  local runtime_input="$5"
  local proof_input="$6"
  local output="${7:-${tmp}/${name}-output}"
  if BUILDKITE_BUILD_ID="${buildkite_build_id}" \
    BUILDKITE_BUILD_NUMBER="${buildkite_build_number}" \
    bash "${assembler}" \
      "${core_input}" "${core_authority_input}" \
      "${runtime_input}" "${proof_input}" "${output}" \
    > "${tmp}/${name}.out" 2> "${tmp}/${name}.err"; then
    printf 'assembler accepted invalid input: %s\n' "${name}" >&2
    exit 1
  fi
  grep -Fq "${expected}" "${tmp}/${name}.err" || {
    printf 'unexpected assembler failure for %s\n' "${name}" >&2
    cat "${tmp}/${name}.err" >&2
    exit 1
  }
  if [[ "${name}" != "existing" ]]; then
    test ! -e "${output}"
  fi
}

refresh_receipt_authority_sha() {
  local proof_root="$1"
  local platform="$2"
  python3 - \
    "${proof_root}/${platform}/ctx-${platform}.semantic-execution.json" \
    "${proof_root}/${platform}/ctx-${platform}.semantic-execution.artifact-authority.json" <<'PY'
import hashlib
import json
import sys

receipt, authority = sys.argv[1:]
with open(receipt, "rb") as source:
    digest = hashlib.sha256(source.read()).hexdigest()
with open(authority, encoding="utf-8") as source:
    value = json.load(source)
value["artifact_sha256"] = digest
with open(authority, "w", encoding="utf-8") as output:
    json.dump(value, output, sort_keys=True, separators=(",", ":"))
    output.write("\n")
PY
}

output="${tmp}/release"
if CTX_PUBLIC_RELEASE_SOURCE_COMMIT="${source_commit}" \
  BUILDKITE_BUILD_ID="${buildkite_build_id}" \
  BUILDKITE_BUILD_NUMBER="${buildkite_build_number}" \
  bash "${assembler}" \
    "${core}" "${core_authority}" "${runtime}" "${proof}" \
    "${tmp}/caller-commit-output" \
  > "${tmp}/caller-commit.out" 2> "${tmp}/caller-commit.err"; then
  printf 'assembler trusted a caller-provided source commit\n' >&2
  exit 1
fi
grep -Fq \
  'caller-provided CTX_PUBLIC_RELEASE_SOURCE_COMMIT is not accepted' \
  "${tmp}/caller-commit.err"
test ! -e "${tmp}/caller-commit-output"

BUILDKITE_BUILD_ID="${buildkite_build_id}" \
BUILDKITE_BUILD_NUMBER="${buildkite_build_number}" \
  bash "${assembler}" \
    "${core}" "${core_authority}" "${runtime}" "${proof}" "${output}"
test "$(find "${output}" -maxdepth 1 -type f | wc -l)" -eq 21
test "$(wc -l < "${output}/SHA256SUMS")" -eq 20
(
  cd "${output}"
  sha256sum -c SHA256SUMS >/dev/null
)
test -x "${output}/ctx-linux-x64"
test ! -x "${output}/ctx-onnxruntime-linux-x64.tar.gz"

wrong_commit_authority="${tmp}/wrong-commit-authority"
cp -a "${core_authority}" "${wrong_commit_authority}"
python3 - \
  "${wrong_commit_authority}/ctx-core-github-handoff.json" \
  "${wrong_commit_authority}/ctx-core-github-handoff.json.sha256" <<'PY'
import hashlib
import json
import sys

handoff, sidecar = sys.argv[1:]
with open(handoff, encoding="utf-8") as source:
    value = json.load(source)
value["source_commit"] = "f" * 40
raw = (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()
with open(handoff, "wb") as output:
    output.write(raw)
with open(sidecar, "w", encoding="ascii") as output:
    output.write(hashlib.sha256(raw).hexdigest() + "\n")
PY
expect_failure wrong-commit \
  'Core authority handoff does not bind scrubbed checkout HEAD' \
  "${core}" "${wrong_commit_authority}" "${runtime}" "${proof}"

if GIT_DIR="${tmp}/hostile-git-dir" \
  BUILDKITE_BUILD_ID="${buildkite_build_id}" \
  BUILDKITE_BUILD_NUMBER="${buildkite_build_number}" \
  bash "${assembler}" \
    "${core}" "${core_authority}" "${runtime}" "${proof}" \
    "${tmp}/hostile-git-output" \
  > "${tmp}/hostile-git.out" 2> "${tmp}/hostile-git.err"; then
  printf 'assembler accepted a hostile Git repository override\n' >&2
  exit 1
fi
grep -Fq 'hostile Git repository override is not accepted: GIT_DIR' \
  "${tmp}/hostile-git.err"
test ! -e "${tmp}/hostile-git-output"

expect_failure existing \
  'release publication destination already exists' \
  "${core}" "${core_authority}" "${runtime}" "${proof}" "${output}"

bad_runtime="${tmp}/bad-runtime"
cp -a "${runtime}" "${bad_runtime}"
printf 'corrupt\n' >> "${bad_runtime}/ctx-onnxruntime-linux-x64.tar.gz"
expect_failure bad-runtime \
  'runtime checksum mismatch for ctx-onnxruntime-linux-x64.tar.gz' \
  "${core}" "${core_authority}" "${bad_runtime}" "${proof}"

mixed_core="${tmp}/mixed-core"
cp -a "${core}" "${mixed_core}"
printf 'different Core bytes\n' > "${mixed_core}/ctx-linux-x64"
python3 - "${mixed_core}/SHA256SUMS" "${mixed_core}/ctx-linux-x64" <<'PY'
import hashlib
import sys
from pathlib import Path

sums, artifact = map(Path, sys.argv[1:])
lines = sums.read_text(encoding="ascii").splitlines()
digest = hashlib.sha256(artifact.read_bytes()).hexdigest()
sums.write_text(
    "\n".join(
        f"{digest}  ctx-linux-x64" if line.endswith("  ctx-linux-x64") else line
        for line in lines
    )
    + "\n",
    encoding="ascii",
)
PY
expect_failure mixed-core \
  'Core assets and source-bound authority use different SHA256SUMS' \
  "${mixed_core}" "${core_authority}" "${runtime}" "${proof}"

symlink_runtime="${tmp}/symlink-runtime"
cp -a "${runtime}" "${symlink_runtime}"
rm "${symlink_runtime}/ctx-onnxruntime-windows-x64.zip"
ln -s "${runtime}/ctx-onnxruntime-windows-x64.zip" \
  "${symlink_runtime}/ctx-onnxruntime-windows-x64.zip"
expect_failure symlink-runtime \
  'runtime release asset must be a regular non-symlink file' \
  "${core}" "${core_authority}" "${symlink_runtime}" "${proof}"

missing_evidence="${tmp}/missing-evidence"
cp -a "${runtime}" "${missing_evidence}"
rm "${missing_evidence}/ctx-onnxruntime-macos-arm64.release-attestation.cms"
expect_failure missing-evidence \
  'macOS runtime release evidence must be a regular non-symlink file' \
  "${core}" "${core_authority}" "${missing_evidence}" "${proof}"

tampered_evidence="${tmp}/tampered-evidence"
cp -a "${runtime}" "${tampered_evidence}"
printf 'tampered-cms\n' \
  > "${tampered_evidence}/ctx-onnxruntime-macos-arm64.release-attestation.cms"
expect_failure tampered-evidence \
  'macOS release attestation CMS signature verification failed' \
  "${core}" "${core_authority}" "${tampered_evidence}" "${proof}"

different_team="${tmp}/different-team"
cp -a "${runtime}" "${different_team}"
python3 - \
  "${different_team}/ctx-onnxruntime-macos-arm64.signing.json" \
  "${different_team}/ctx-onnxruntime-macos-arm64.attestation.json" \
  "${different_team}/ctx-onnxruntime-macos-arm64.release-attestation.json" <<'PY'
import json
import sys

signing_path, artifact_path, release_path = sys.argv[1:]
authority = "Developer ID Application: Other Publisher (OTHERTEAM1)"
with open(signing_path, encoding="utf-8") as source:
    signing = json.load(source)
signing["codesign"]["authority"] = authority
signing["codesign"]["team_identifier"] = "OTHERTEAM1"
with open(signing_path, "w", encoding="utf-8") as output:
    json.dump(signing, output, sort_keys=True, separators=(",", ":"))
    output.write("\n")
for path in (artifact_path, release_path):
    with open(path, encoding="utf-8") as source:
        statement = json.load(source)
    statement["codesign_authority"] = authority
    statement["team_identifier"] = "OTHERTEAM1"
    with open(path, "w", encoding="utf-8") as output:
        json.dump(statement, output, sort_keys=True, separators=(",", ":"))
        output.write("\n")
PY
CTX_TEST_FAKE_SIGNER_TEAM_ID=OTHERTEAM1 expect_failure different-team \
  'macOS attestation signer does not match the pinned project release publisher' \
  "${core}" "${core_authority}" "${different_team}" "${proof}"

missing_receipt="${tmp}/missing-receipt"
cp -a "${proof}" "${missing_receipt}"
rm "${missing_receipt}/macos-x64/ctx-macos-x64.semantic-execution.json"
expect_failure missing-receipt \
  'macos-x64 native semantic receipt is unavailable' \
  "${core}" "${core_authority}" "${runtime}" "${missing_receipt}"

mismatched_receipt="${tmp}/mismatched-receipt"
cp -a "${proof}" "${mismatched_receipt}"
python3 - "${mismatched_receipt}/macos-arm64/ctx-macos-arm64.semantic-execution.json" <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, encoding="utf-8") as source:
    value = json.load(source)
value["cli_artifact"]["sha256"] = "0" * 64
with open(path, "w", encoding="utf-8") as output:
    json.dump(value, output, sort_keys=True, separators=(",", ":"))
    output.write("\n")
PY
refresh_receipt_authority_sha "${mismatched_receipt}" macos-arm64
expect_failure mismatched-receipt \
  'native semantic receipt does not bind the exact final macos-arm64 CLI/runtime bytes' \
  "${core}" "${core_authority}" "${runtime}" "${mismatched_receipt}"

replayed_receipt="${tmp}/replayed-receipt"
cp -a "${proof}" "${replayed_receipt}"
python3 - \
  "${replayed_receipt}/macos-arm64/ctx-macos-arm64.semantic-execution.json" \
  "${replayed_receipt}/macos-arm64/ctx-macos-arm64.semantic-execution.artifact-authority.json" <<'PY'
import json
import sys

receipt, authority = sys.argv[1:]
old_build = "33333333-3333-4333-8333-333333333333"
for path in (receipt, authority):
    with open(path, encoding="utf-8") as source:
        value = json.load(source)
    if path == receipt:
        value["provenance"]["build_id"] = old_build
    else:
        value["build_id"] = old_build
    with open(path, "w", encoding="utf-8") as output:
        json.dump(value, output, sort_keys=True, separators=(",", ":"))
        output.write("\n")
PY
refresh_receipt_authority_sha "${replayed_receipt}" macos-arm64
expect_failure replayed-receipt \
  'macos-arm64 semantic receipt lacks exact Buildkite artifact authority' \
  "${core}" "${core_authority}" "${runtime}" "${replayed_receipt}"

forged_receipt="${tmp}/forged-receipt"
cp -a "${proof}" "${forged_receipt}"
python3 - "${forged_receipt}/macos-x64/ctx-macos-x64.semantic-execution.json" <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, encoding="utf-8") as source:
    value = json.load(source)
value["provenance"]["job_id"] = "44444444-4444-4444-8444-444444444444"
with open(path, "w", encoding="utf-8") as output:
    json.dump(value, output, sort_keys=True, separators=(",", ":"))
    output.write("\n")
PY
refresh_receipt_authority_sha "${forged_receipt}" macos-x64
expect_failure forged-receipt \
  'macos-x64 semantic receipt provenance is replayed or forged' \
  "${core}" "${core_authority}" "${runtime}" "${forged_receipt}"

virtualized_arm64="${tmp}/virtualized-arm64"
cp -a "${proof}" "${virtualized_arm64}"
python3 - "${virtualized_arm64}/macos-arm64/ctx-macos-arm64.semantic-execution.json" <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, encoding="utf-8") as source:
    value = json.load(source)
value["host_evidence"]["hypervisor"] = "present"
with open(path, "w", encoding="utf-8") as output:
    json.dump(value, output, sort_keys=True, separators=(",", ":"))
    output.write("\n")
PY
refresh_receipt_authority_sha "${virtualized_arm64}" macos-arm64
expect_failure virtualized-arm64 \
  'native semantic receipt has non-authoritative macos-arm64 host evidence' \
  "${core}" "${core_authority}" "${runtime}" "${virtualized_arm64}"

printf 'GitHub release final assembly tests passed\n'
