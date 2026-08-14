#!/usr/bin/env bash
set -euo pipefail
case "$-" in
  *x*) set +x ;;
esac

usage() {
  cat >&2 <<'USAGE'
Usage:
  scripts/run-windows-release-signing.sh --preflight
  scripts/run-windows-release-signing.sh KIND ARTIFACT [EVIDENCE_DIR]

Signs one canonical Windows CLI or helper from Linux with the Azure Artifact
Signing Public Trust profile. KIND is cli or helper. Azure credentials are
acquired only inside this launcher and are exchanged for a short-lived signing
token before Jsign is invoked.
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "missing required Windows signing tool: $1"
}

sha256_file() {
  sha256sum "$1" | awk '{print $1}'
}

mode=sign
case "${1:-}" in
  --preflight)
    [[ $# -eq 1 ]] || { usage; exit 2; }
    mode=preflight
    ;;
  *)
    kind="${1:-}"
    artifact="${2:-}"
    evidence_dir="${3:-target/public-cli-artifacts}"
    [[ -n "${kind}" && -n "${artifact}" && $# -le 3 ]] || { usage; exit 2; }
    ;;
esac

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
contract="${root_dir}/contracts/windows-authenticode-v1.json"
[[ -f "${contract}" && ! -L "${contract}" ]] || die "Windows signing contract is unavailable"

readarray -t contract_values < <(python3 -I - "${contract}" <<'PY'
import json, re, sys
from pathlib import Path
value=json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
expected={"account","authority","certificate_profile","code_signing_endpoint","credential","expected_common_name","expected_organization","jsign","schema_version","timestamp_url"}
if set(value) != expected or value.get("schema_version") != 1:
    raise SystemExit("Windows signing contract has an unsupported shape")
credential=value["credential"]
jsign=value["jsign"]
if set(credential) != {"client_id_key","client_secret_key","environment","path","project_id","tenant_id_key"}:
    raise SystemExit("Windows signing credential contract has an unsupported shape")
if set(jsign) != {"sha256","url","version"} or not re.fullmatch(r"[0-9a-f]{64}", jsign["sha256"]):
    raise SystemExit("Windows signing Jsign contract is malformed")
for item in (value["account"],value["authority"],value["certificate_profile"],value["code_signing_endpoint"],value["expected_common_name"],value["expected_organization"],value["timestamp_url"],credential["project_id"],credential["environment"],credential["path"],credential["tenant_id_key"],credential["client_id_key"],credential["client_secret_key"],jsign["version"],jsign["url"],jsign["sha256"]):
    if not isinstance(item,str) or not item or "\n" in item:
        raise SystemExit("Windows signing contract contains an invalid string")
print(value["account"])
print(value["authority"])
print(value["certificate_profile"])
print(value["code_signing_endpoint"])
print(value["expected_common_name"])
print(value["expected_organization"])
print(value["timestamp_url"])
print(credential["project_id"])
print(credential["environment"])
print(credential["path"])
print(credential["tenant_id_key"])
print(credential["client_id_key"])
print(credential["client_secret_key"])
print(jsign["version"])
print(jsign["url"])
print(jsign["sha256"])
PY
)
[[ "${#contract_values[@]}" == 16 ]] || die "Windows signing contract could not be loaded"
account="${contract_values[0]}"
authority="${contract_values[1]}"
profile="${contract_values[2]}"
endpoint="${contract_values[3]}"
expected_common_name="${contract_values[4]}"
expected_organization="${contract_values[5]}"
timestamp_url="${contract_values[6]}"
infisical_project="${contract_values[7]}"
infisical_environment="${contract_values[8]}"
infisical_path="${contract_values[9]}"
tenant_key="${contract_values[10]}"
client_key="${contract_values[11]}"
client_secret_key="${contract_values[12]}"
jsign_version="${contract_values[13]}"
jsign_sha256="${contract_values[15]}"
jsign_jar="${CTX_WINDOWS_JSIGN_JAR:-}"

[[ "$(uname -s)" == "Linux" ]] || die "Windows release signing requires Linux"
for command_name in curl git java python3 sha256sum shred; do
  require_command "${command_name}"
done
[[ -n "${jsign_jar}" && -f "${jsign_jar}" && ! -L "${jsign_jar}" ]] || \
  die "CTX_WINDOWS_JSIGN_JAR must name the verified Jsign jar"
jsign_jar="$(CDPATH= cd "$(dirname "${jsign_jar}")" && pwd -P)/$(basename "${jsign_jar}")"
[[ "$(sha256_file "${jsign_jar}")" == "${jsign_sha256}" ]] || die "Jsign SHA-256 mismatch"
[[ "$(java -jar "${jsign_jar}" --version)" == "Jsign ${jsign_version}" ]] || die "Jsign version mismatch"
"${root_dir}/scripts/check-windows-signing-trusted-ref.sh" >/dev/null

if [[ "${mode}" == preflight ]]; then
  printf 'Windows signing preflight ok: trusted source, Jsign %s, and Linux tools\n' "${jsign_version}"
  exit 0
fi

case "${kind}" in
  cli) expected_artifact_name="ctx.exe" ;;
  helper) expected_artifact_name="ctx-pro-windows-x64.exe" ;;
  *) die "Windows signing kind must be cli or helper" ;;
esac
[[ "${artifact##*/}" == "${expected_artifact_name}" ]] || \
  die "Windows release artifact must be named ${expected_artifact_name}"
[[ -f "${artifact}" && ! -L "${artifact}" ]] || die "Windows release artifact must be a regular non-symlink file"
mkdir -p "${evidence_dir}"
[[ -d "${evidence_dir}" && ! -L "${evidence_dir}" ]] || die "Windows signing evidence directory is unsafe"
evidence="${evidence_dir%/}/${expected_artifact_name}.authenticode.json"
[[ ! -e "${evidence}" && ! -L "${evidence}" ]] || die "Windows signing evidence already exists"
python3 - "${artifact}" <<'PY'
from pathlib import Path
import struct, sys
path=Path(sys.argv[1])
raw=path.read_bytes()
if len(raw) < 0x40 or raw[:2] != b"MZ":
    raise SystemExit("Windows signing input is not a PE executable")
pe=struct.unpack_from("<I", raw, 0x3c)[0]
if pe + 24 > len(raw) or raw[pe:pe+4] != b"PE\0\0":
    raise SystemExit("Windows signing input has an invalid PE header")
optional=pe+24
if optional + 152 > len(raw) or struct.unpack_from("<H", raw, optional)[0] != 0x20b:
    raise SystemExit("Windows signing input is not a complete PE32+ executable")
offset,size=struct.unpack_from("<II", raw, optional+112+4*8)
if offset != 0 or size != 0:
    raise SystemExit("Windows signing input is already signed")
PY

if env | cut -d= -f1 | grep -Eq '^(AZURE_CLIENT_SECRET|AZURE_ARTIFACT_SIGNING_CLIENT_SECRET|CTX_WINDOWS_SIGNING_LAUNCHED)$'; then
  die "caller supplied a forbidden ambient Windows signing value"
fi

secret_source="${CTX_WINDOWS_SIGNING_SECRET_SOURCE:-infisical}"
case "${secret_source}" in
  infisical) require_command infisical ;;
  injected) ;;
  *) die "CTX_WINDOWS_SIGNING_SECRET_SOURCE must be infisical or injected" ;;
esac

umask 077
secret_root="$(mktemp -d "${TMPDIR:-/tmp}/ctx-windows-signing-launcher.XXXXXX")"
chmod 0700 "${secret_root}"
cleanup() {
  find "${secret_root}" -type f -exec shred -u {} + >/dev/null 2>&1 || true
  rm -rf "${secret_root}" >/dev/null 2>&1 || true
  rm -f "${artifact_temporary:-}" >/dev/null 2>&1 || true
}
trap cleanup EXIT

normalize_secret() {
  local source="$1" output="$2" label="$3"
  python3 - "${source}" "${output}" "${label}" <<'PY'
from pathlib import Path
import os, sys
value=Path(sys.argv[1]).read_text(encoding="utf-8").strip()
if not value or "\x00" in value or "\n" in value:
    raise SystemExit(f"invalid {sys.argv[3]} signing value")
Path(sys.argv[2]).write_text(value, encoding="utf-8")
os.chmod(sys.argv[2], 0o600)
PY
}

fetch_secret() {
  local name="$1" output="$2" raw injected_path=""
  raw="${secret_root}/${name}.raw"
  case "${secret_source}" in
    infisical)
      infisical secrets get "${name}" --plain \
        --projectId "${infisical_project}" --env "${infisical_environment}" \
        --path "${infisical_path}" --silent >"${raw}" 2>/dev/null || \
        die "Infisical lookup failed for required Windows signing value ${name}"
      ;;
    injected)
      case "${name}" in
        "${tenant_key}") injected_path="${CTX_WINDOWS_SIGNING_TENANT_ID_FILE:-}" ;;
        "${client_key}") injected_path="${CTX_WINDOWS_SIGNING_CLIENT_ID_FILE:-}" ;;
        "${client_secret_key}") injected_path="${CTX_WINDOWS_SIGNING_CLIENT_SECRET_FILE:-}" ;;
      esac
      [[ -n "${injected_path:-}" && -f "${injected_path}" && ! -L "${injected_path}" ]] || \
        die "injected Windows signing file is missing for ${name}"
      install -m 0600 -- "${injected_path}" "${raw}"
      ;;
  esac
  normalize_secret "${raw}" "${output}" "${name}"
}

tenant_file="${secret_root}/tenant-id"
client_file="${secret_root}/client-id"
client_secret_file="${secret_root}/client-secret"
fetch_secret "${tenant_key}" "${tenant_file}"
fetch_secret "${client_key}" "${client_file}"
fetch_secret "${client_secret_key}" "${client_secret_file}"
tenant_id="$(<"${tenant_file}")"
[[ "${tenant_id}" =~ ^[0-9a-fA-F-]{36}$ ]] || die "Azure signing tenant ID is malformed"

token_response="${secret_root}/token-response.json"
access_token="${secret_root}/access-token"
curl --fail --silent --show-error --request POST \
  "https://login.microsoftonline.com/${tenant_id}/oauth2/v2.0/token" \
  --header 'Content-Type: application/x-www-form-urlencoded' \
  --data-urlencode "client_id@${client_file}" \
  --data-urlencode "client_secret@${client_secret_file}" \
  --data-urlencode 'scope=https://codesigning.azure.net/.default' \
  --data-urlencode 'grant_type=client_credentials' \
  --output "${token_response}" || die "Azure signing token request failed"
python3 - "${token_response}" "${access_token}" <<'PY'
import json, os, sys
try:
    value=json.load(open(sys.argv[1], encoding="utf-8")).get("access_token", "")
except (OSError, ValueError) as error:
    raise SystemExit("Azure signing token response is invalid") from error
if not isinstance(value,str) or not value:
    raise SystemExit("Azure signing token response has no access token")
with open(sys.argv[2], "w", encoding="utf-8") as output:
    output.write(value)
os.chmod(sys.argv[2], 0o600)
PY

endpoint_host="${endpoint#https://}"
endpoint_host="${endpoint_host%/}"
signing_log="${secret_root}/jsign.log"
signed_work="${secret_root}/${expected_artifact_name}"
install -m 0755 -- "${artifact}" "${signed_work}"
java_path="$(command -v java)"
if ! env -i PATH="/usr/bin:/bin" LANG=C.UTF-8 \
  "${java_path}" -jar "${jsign_jar}" sign \
    --storetype TRUSTEDSIGNING --keystore "${endpoint_host}" \
    --storepass "file:${access_token}" --alias "${account}/${profile}" \
    --alg SHA-256 --name ctx --url https://ctx.rs \
    --tsaurl "${timestamp_url}" --tsmode RFC3161 --tsretries 3 --tsretrywait 5 \
    "${signed_work}" >"${signing_log}" 2>&1; then
  die "Azure Artifact Signing failed"
fi

evidence_temporary="${secret_root}/ctx.exe.authenticode.json"
java --source 11 --class-path "${jsign_jar}" \
  "${root_dir}/scripts/release/WindowsAuthenticodeInspect.java" \
  "${signed_work}" "${evidence_temporary}" "${authority}" "${account}" "${profile}" \
  "${endpoint}" "${expected_common_name}" "${expected_organization}" \
  "${jsign_sha256}" "${timestamp_url}"
install -m 0644 -- "${evidence_temporary}" "${evidence}"
artifact_temporary="${artifact}.signed.$$"
[[ ! -e "${artifact_temporary}" && ! -L "${artifact_temporary}" ]] || \
  die "temporary signed artifact path already exists"
install -m 0755 -- "${signed_work}" "${artifact_temporary}"
mv -- "${artifact_temporary}" "${artifact}"
artifact_temporary=""
printf 'Windows Authenticode signing complete: %s\n' "${artifact}"
