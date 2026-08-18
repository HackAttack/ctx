#!/usr/bin/env python3
"""Validate the public V1 managed Core/companion contract boundary."""

from __future__ import annotations

import base64
from collections.abc import Mapping
import hashlib
import hmac
import importlib.util
import json
from pathlib import Path
import re
import sys
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
AUTHORITY_PATH = ROOT / "contracts" / "ctx-managed-pair-release-authority-v1.json"
MANIFEST_SCHEMA_PATH = ROOT / "contracts" / "ctx-managed-pair-manifest-v1.schema.json"
RELEASE_SET_SCHEMA_PATH = ROOT / "contracts" / "ctx-managed-pair-release-set-v1.schema.json"
STATE_SCHEMA_PATH = ROOT / "contracts" / "ctx-managed-pair-state-v1.schema.json"
MATRIX_PATH = ROOT / "contracts" / "release-targets-v1.json"
MATRIX_CHECKER_PATH = ROOT / "scripts" / "check-release-target-matrix.py"
TARGET_IDS = (
    "linux-arm64",
    "linux-x64",
    "macos-arm64",
    "macos-x64",
    "windows-x64",
)
SHA256 = re.compile(r"[0-9a-f]{64}\Z")
NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9._+-]{0,127}\Z")
RUST_TARGET = re.compile(r"[a-z0-9_]+(?:-[a-z0-9_]+){2,5}\Z")
BASE64 = re.compile(r"(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?\Z")
MAX_COMPONENT_BYTES = 268435456


class ContractError(ValueError):
    """A managed-pair document violates its public contract."""


def unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            raise ContractError(f"JSON object contains duplicate field {key!r}")
        value[key] = item
    return value


def load_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(
            path.read_text(encoding="utf-8"),
            object_pairs_hook=unique_object,
            parse_constant=lambda _: (_ for _ in ()).throw(ValueError()),
        )
    except (OSError, json.JSONDecodeError) as error:
        raise ContractError(f"cannot read {path}: {error}") from error
    if not isinstance(value, dict):
        raise ContractError(f"{path.name} must contain an object")
    return value


def require_keys(value: Any, keys: set[str], label: str) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != keys:
        raise ContractError(f"{label} has missing or unexpected fields")
    return value


def require_string(value: Any, label: str, pattern: re.Pattern[str] | None = None) -> str:
    if not isinstance(value, str) or not value or (pattern is not None and pattern.fullmatch(value) is None):
        raise ContractError(f"{label} is malformed")
    return value


def require_sha256(value: Any, label: str) -> str:
    return require_string(value, label, SHA256)


def require_size(value: Any, label: str, maximum: int) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or not 1 <= value <= maximum:
        raise ContractError(f"{label} must be a bounded positive size")
    return value


def require_component_size(value: Any, label: str) -> int:
    return require_size(value, label, MAX_COMPONENT_BYTES)


def require_object_key(value: Any, digest: str, name: str, label: str) -> str:
    key = require_string(value, label)
    expected = f"sha256/{digest}/{name}"
    if key != expected:
        raise ContractError(f"{label} must be the immutable relative key {expected}")
    return key


def load_matrix() -> dict[str, Any]:
    spec = importlib.util.spec_from_file_location("release_target_matrix", MATRIX_CHECKER_PATH)
    if spec is None or spec.loader is None:
        raise ContractError("cannot load release target matrix checker")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    try:
        return module.load_and_validate(MATRIX_PATH)
    except (OSError, ValueError) as error:
        raise ContractError(f"release target matrix is invalid: {error}") from error


def target_matrix_sha256() -> str:
    return hashlib.sha256(MATRIX_PATH.read_bytes()).hexdigest()


def validate_authority_registry(value: dict[str, Any]) -> dict[str, dict[str, str]]:
    require_keys(value, {"contract", "schema_version", "channels"}, "release authority registry")
    if value["contract"] != "ctx-managed-pair-release-authority" or value["schema_version"] != 1:
        raise ContractError("release authority registry must use the V1 envelope")
    channels = value["channels"]
    if not isinstance(channels, list) or len(channels) != 2:
        raise ContractError("release authority registry must contain stable then staging")
    result: dict[str, dict[str, str]] = {}
    for expected_id, entry in zip(("stable", "staging"), channels, strict=True):
        entry = require_keys(
            entry,
            {"id", "key_id", "signature_algorithm", "public_key_der_sha256", "public_key_pem"},
            f"{expected_id} release authority",
        )
        if entry["id"] != expected_id:
            raise ContractError("release authority channels must be stable then staging")
        key_id = require_string(entry["key_id"], f"{expected_id} key ID", NAME)
        if entry["signature_algorithm"] != "rsa-pkcs1v15-sha256":
            raise ContractError(f"{expected_id} release authority has an unsupported signature algorithm")
        digest = require_sha256(entry["public_key_der_sha256"], f"{expected_id} public key fingerprint")
        pem = require_string(entry["public_key_pem"], f"{expected_id} public key PEM")
        lines = pem.splitlines()
        if lines[:1] != ["-----BEGIN RSA PUBLIC KEY-----"] or lines[-1:] != ["-----END RSA PUBLIC KEY-----"]:
            raise ContractError(f"{expected_id} public key is not an RSA public-key PEM")
        try:
            der = base64.b64decode("".join(lines[1:-1]), validate=True)
        except ValueError as error:
            raise ContractError(f"{expected_id} public key PEM is invalid") from error
        if hashlib.sha256(der).hexdigest() != digest:
            raise ContractError(f"{expected_id} public key fingerprint does not match PEM")
        result[expected_id] = {
            "key_id": key_id,
            "signature_algorithm": entry["signature_algorithm"],
            "public_key_pem": pem,
        }
    return result


def load_authority_registry(path: Path = AUTHORITY_PATH) -> dict[str, dict[str, str]]:
    return validate_authority_registry(load_json(path))


def validate_snapshot(value: Any, label: str) -> None:
    value = require_keys(value, {"contract", "fingerprint"}, label)
    if value["contract"] != "ctx-managed-pair-snapshot-v1":
        raise ContractError(f"{label} has an unsupported snapshot contract")
    require_sha256(value["fingerprint"], f"{label} fingerprint")


def validate_compatibility(value: Any, label: str) -> None:
    value = require_keys(value, {"invocation_fingerprint", "core_capability_fingerprint"}, label)
    require_sha256(value["invocation_fingerprint"], f"{label} invocation fingerprint")
    require_sha256(value["core_capability_fingerprint"], f"{label} Core-capability fingerprint")


def validate_state(value: dict[str, Any]) -> None:
    value = require_keys(
        value,
        {"contract", "schema_version", "identity", "envelope_sha256", "envelope_size_bytes"},
        "managed pair installed state",
    )
    if value["contract"] != "ctx-managed-pair-state" or value["schema_version"] != 1:
        raise ContractError("managed pair installed state must use the V1 envelope")
    identity = require_keys(
        value["identity"],
        {"release_name", "target", "rollback_generation", "manifest_sha256", "core", "companion"},
        "managed pair installed identity",
    )
    require_string(identity["release_name"], "installed release name", NAME)
    if identity["target"] not in TARGET_IDS:
        raise ContractError("managed pair installed state has an unsupported target")
    generation = identity["rollback_generation"]
    if not isinstance(generation, int) or isinstance(generation, bool) or not 1 <= generation <= 9007199254740991:
        raise ContractError("managed pair installed state has an invalid rollback generation")
    require_sha256(identity["manifest_sha256"], "installed manifest hash")
    for component in ("core", "companion"):
        component_identity = require_keys(
            identity[component],
            {"sha256", "size_bytes"},
            f"installed {component} identity",
        )
        require_sha256(component_identity["sha256"], f"installed {component} hash")
        require_component_size(component_identity["size_bytes"], f"installed {component} size")
    require_sha256(value["envelope_sha256"], "installed envelope hash")
    require_size(value["envelope_size_bytes"], "installed envelope size", 2097152)


def validate_component(
    value: Any,
    *,
    component: str,
    target: dict[str, Any],
    install_slot: str,
) -> None:
    value = require_keys(
        value,
        {"artifact_name", "object_key", "sha256", "size_bytes", "install_slot", "build_identity"},
        f"{component} component",
    )
    artifact_name = require_string(value["artifact_name"], f"{component} artifact name", NAME)
    expected_artifact = target["public_artifact"] if component == "core" else target["helper_artifact"]
    if artifact_name != expected_artifact:
        raise ContractError(f"{component} artifact name does not match the fixed target matrix")
    digest = require_sha256(value["sha256"], f"{component} hash")
    require_object_key(value["object_key"], digest, artifact_name, f"{component} object key")
    require_component_size(value["size_bytes"], f"{component} size")
    if value["install_slot"] != install_slot:
        raise ContractError(f"{component} install slot is not the official managed slot")
    identity = require_keys(
        value["build_identity"],
        {"component", "rust_target", "source_revision", "build_fingerprint"},
        f"{component} build identity",
    )
    if identity["component"] != component:
        raise ContractError(f"{component} build identity has the wrong component")
    expected_rust_target = target["public_rust_target"]
    if identity["rust_target"] != expected_rust_target or RUST_TARGET.fullmatch(identity["rust_target"]) is None:
        raise ContractError(f"{component} build identity is not bound to its target triple")
    require_string(identity["source_revision"], f"{component} source revision", re.compile(r"[0-9a-f]{40}\Z"))
    require_sha256(identity["build_fingerprint"], f"{component} build fingerprint")


def validate_manifest(
    value: dict[str, Any],
    *,
    retained_rollback_generation: int | None = None,
    matrix: dict[str, Any] | None = None,
    authorities: dict[str, dict[str, str]] | None = None,
) -> None:
    matrix = load_matrix() if matrix is None else matrix
    authorities = load_authority_registry() if authorities is None else authorities
    require_keys(
        value,
        {"contract", "schema_version", "channel", "release_authority_key_id", "release_name", "target", "install_geometry", "target_matrix_sha256", "rollback_generation", "snapshot", "compatibility", "components"},
        "managed pair manifest",
    )
    if value["contract"] != "ctx-managed-pair-manifest" or value["schema_version"] != 1:
        raise ContractError("managed pair manifest must use the V1 envelope")
    channel = value["channel"]
    if channel not in authorities:
        raise ContractError("managed pair manifest has an unsupported release channel")
    if value["release_authority_key_id"] != authorities[channel]["key_id"]:
        raise ContractError("managed pair manifest authority key ID does not match its channel")
    require_string(value["release_name"], "release name", NAME)
    require_sha256(value["target_matrix_sha256"], "target-matrix hash")
    if value["target_matrix_sha256"] != target_matrix_sha256():
        raise ContractError("target-matrix hash does not bind this fixed matrix")
    generation = require_size(value["rollback_generation"], "rollback generation", 9007199254740991)
    if retained_rollback_generation is not None and generation < retained_rollback_generation:
        raise ContractError("rollback generation is lower than retained state")
    target_value = require_keys(value["target"], {"id", "os", "arch", "core_rust_target", "companion_rust_target"}, "target")
    targets = {target["id"]: target for target in matrix["targets"]}
    target_id = target_value["id"]
    if target_id not in targets:
        raise ContractError("managed pair manifest has an unsupported target")
    target = targets[target_id]
    for field in ("os", "arch"):
        if target_value[field] != target[field]:
            raise ContractError(f"target {field} does not match the fixed target matrix")
    if target_value["core_rust_target"] != target["public_rust_target"]:
        raise ContractError("Core target triple does not match the fixed target matrix")
    if target_value["companion_rust_target"] != target["public_rust_target"]:
        raise ContractError("companion target triple does not match the official companion target")
    geometry = require_keys(value["install_geometry"], {"install_root", "managed_bin_dir", "core_slot", "companion_slot"}, "install geometry")
    if geometry != {
        "install_root": "<install-root>",
        "managed_bin_dir": "<install-root>/bin",
        "core_slot": f"<install-root>/{target['managed_pair_core_slot']}",
        "companion_slot": f"<install-root>/{target['managed_pair_companion_slot']}",
    }:
        raise ContractError("managed pair must use the fixed install root, bin directory, and component slots")
    validate_snapshot(value["snapshot"], "snapshot")
    validate_compatibility(value["compatibility"], "compatibility")
    components = require_keys(value["components"], {"core", "companion"}, "components")
    validate_component(components["core"], component="core", target=target, install_slot=geometry["core_slot"])
    validate_component(components["companion"], component="companion", target=target, install_slot=geometry["companion_slot"])


def validate_release_set(
    value: dict[str, Any],
    *,
    retained_rollback_generation: int | None = None,
    authorities: dict[str, dict[str, str]] | None = None,
) -> None:
    authorities = load_authority_registry() if authorities is None else authorities
    require_keys(
        value,
        {"contract", "schema_version", "channel", "release_authority_key_id", "release_name", "target_matrix_sha256", "rollback_generation", "snapshot", "compatibility", "target_manifests"},
        "managed pair release set",
    )
    if value["contract"] != "ctx-managed-pair-release-set" or value["schema_version"] != 1:
        raise ContractError("managed pair release set must use the V1 envelope")
    channel = value["channel"]
    if channel not in authorities:
        raise ContractError("managed pair release set has an unsupported release channel")
    if value["release_authority_key_id"] != authorities[channel]["key_id"]:
        raise ContractError("managed pair release set authority key ID does not match its channel")
    require_string(value["release_name"], "release-set name", NAME)
    if require_sha256(value["target_matrix_sha256"], "release-set target-matrix hash") != target_matrix_sha256():
        raise ContractError("release set target-matrix hash does not bind this fixed matrix")
    generation = require_size(value["rollback_generation"], "release-set rollback generation", 9007199254740991)
    if retained_rollback_generation is not None and generation < retained_rollback_generation:
        raise ContractError("release-set rollback generation is lower than retained state")
    validate_snapshot(value["snapshot"], "release-set snapshot")
    validate_compatibility(value["compatibility"], "release-set compatibility")
    manifests = value["target_manifests"]
    if not isinstance(manifests, list) or len(manifests) != len(TARGET_IDS):
        raise ContractError("release set must contain exactly five target manifests")
    ids: list[str] = []
    for manifest in manifests:
        manifest = require_keys(
            manifest,
            {"target_id", "manifest_name", "manifest_object_key", "manifest_sha256", "manifest_size_bytes"},
            "release-set target manifest",
        )
        target_id = manifest["target_id"]
        if not isinstance(target_id, str):
            raise ContractError("release-set target ID is malformed")
        ids.append(target_id)
        name = require_string(manifest["manifest_name"], "target manifest name", NAME)
        digest = require_sha256(manifest["manifest_sha256"], "target manifest hash")
        require_object_key(manifest["manifest_object_key"], digest, name, "target manifest object key")
        require_size(manifest["manifest_size_bytes"], "target manifest size", 1048576)
    if tuple(ids) != TARGET_IDS:
        raise ContractError("release set target IDs must be the exact five-target ordered matrix")


def strict_base64(value: Any, label: str, maximum_decoded_bytes: int) -> bytes:
    encoded = require_string(value, label, BASE64)
    try:
        decoded = base64.b64decode(encoded, validate=True)
    except ValueError as error:
        raise ContractError(f"{label} is not valid base64") from error
    if not decoded or len(decoded) > maximum_decoded_bytes:
        raise ContractError(f"{label} is outside its bounded size")
    if base64.b64encode(decoded).decode("ascii") != encoded:
        raise ContractError(f"{label} is not canonical base64")
    return decoded


def der_length(value: bytes, offset: int) -> tuple[int, int]:
    if offset >= len(value):
        raise ContractError("RSA key DER is truncated")
    first = value[offset]
    if first < 128:
        return first, offset + 1
    count = first & 0x7F
    if count == 0 or count > 4 or offset + 1 + count > len(value):
        raise ContractError("RSA key DER has an invalid length")
    length = int.from_bytes(value[offset + 1 : offset + 1 + count], "big")
    if length < 128:
        raise ContractError("RSA key DER uses a non-canonical length")
    return length, offset + 1 + count


def der_tlv(value: bytes, offset: int) -> tuple[int, bytes, int]:
    if offset >= len(value):
        raise ContractError("RSA key DER is truncated")
    tag = value[offset]
    length, start = der_length(value, offset + 1)
    end = start + length
    if end > len(value):
        raise ContractError("RSA key DER is truncated")
    return tag, value[start:end], end


def rsa_public_numbers(pem: str) -> tuple[int, int]:
    lines = pem.splitlines()
    try:
        der = base64.b64decode("".join(lines[1:-1]), validate=True)
        tag, sequence, end = der_tlv(der, 0)
        if tag != 0x30 or end != len(der):
            raise ContractError("RSA public key DER is malformed")
        integers = []
        offset = 0
        while offset < len(sequence):
            tag, encoded, offset = der_tlv(sequence, offset)
            if tag != 0x02 or not encoded:
                raise ContractError("RSA public key DER has a non-integer field")
            integers.append(int.from_bytes(encoded, "big"))
    except ValueError as error:
        raise ContractError("RSA public key PEM is invalid") from error
    if len(integers) != 2 or integers[0] <= 0 or integers[1] <= 1:
        raise ContractError("RSA public key DER has invalid parameters")
    return integers[0], integers[1]


def verify_detached_signature(payload: bytes, signature: bytes, public_key_pem: str) -> bool:
    modulus, exponent = rsa_public_numbers(public_key_pem)
    key_bytes = (modulus.bit_length() + 7) // 8
    if len(signature) != key_bytes:
        return False
    signature_integer = int.from_bytes(signature, "big")
    if signature_integer >= modulus:
        return False
    encoded_message = pow(signature_integer, exponent, modulus).to_bytes(key_bytes, "big")
    digest_info = bytes.fromhex("3031300d060960864801650304020105000420") + hashlib.sha256(payload).digest()
    expected = b"\x00\x01" + b"\xff" * (key_bytes - len(digest_info) - 3) + b"\x00" + digest_info
    return hmac.compare_digest(encoded_message, expected)


def canonical_payload_bytes(value: dict[str, Any]) -> bytes:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True).encode("utf-8")


def validate_component_bound_authority(
    manifest_schema: dict[str, Any], state_schema: dict[str, Any]
) -> None:
    try:
        manifest_maximum = manifest_schema["$defs"]["size"]["maximum"]
        state_maximum = state_schema["$defs"]["component_identity"]["properties"]["size_bytes"]["maximum"]
    except (KeyError, TypeError) as error:
        raise ContractError("managed-pair component size schema is malformed") from error
    if manifest_maximum != MAX_COMPONENT_BYTES or state_maximum != MAX_COMPONENT_BYTES:
        raise ContractError("managed-pair component size authorities must use the exact 256 MiB bound")
    if require_component_size(MAX_COMPONENT_BYTES, "component exact-bound check") != MAX_COMPONENT_BYTES:
        raise ContractError("managed-pair component exact-bound check was not accepted")
    try:
        require_component_size(MAX_COMPONENT_BYTES + 1, "component over-bound check")
    except ContractError:
        pass
    else:
        raise ContractError("managed-pair component over-bound check was accepted")


def validate_envelope(
    value: dict[str, Any],
    *,
    retained_rollback_generation: int | None = None,
    matrix: dict[str, Any] | None = None,
    authorities: dict[str, dict[str, str]] | None = None,
) -> dict[str, Any]:
    authorities = load_authority_registry() if authorities is None else authorities
    require_keys(value, {"schema_version", "manifest_base64", "signature_base64"}, "signed envelope")
    if value["schema_version"] != 1:
        raise ContractError("signed envelope must use schema version 1")
    payload_bytes = strict_base64(value["manifest_base64"], "envelope manifest_base64", 1048576)
    signature = strict_base64(value["signature_base64"], "envelope signature_base64", 16384)
    try:
        payload = json.loads(payload_bytes.decode("utf-8"), parse_constant=lambda _: (_ for _ in ()).throw(ValueError()))
    except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
        raise ContractError("envelope payload is not UTF-8 JSON") from error
    if not isinstance(payload, dict) or canonical_payload_bytes(payload) != payload_bytes:
        raise ContractError("envelope payload is not compact canonical JSON")
    contract = payload.get("contract")
    if contract == "ctx-managed-pair-manifest":
        validate_manifest(payload, retained_rollback_generation=retained_rollback_generation, matrix=matrix, authorities=authorities)
    elif contract == "ctx-managed-pair-release-set":
        validate_release_set(payload, retained_rollback_generation=retained_rollback_generation, authorities=authorities)
    else:
        raise ContractError("envelope payload has an unsupported contract")
    channel = payload["channel"]
    if not verify_detached_signature(payload_bytes, signature, authorities[channel]["public_key_pem"]):
        raise ContractError("envelope signature does not verify the exact decoded payload bytes")
    return payload


def validate_release_bundle(
    release_set_envelope: dict[str, Any],
    manifest_envelopes_by_object_key: Mapping[str, bytes],
    *,
    retained_rollback_generation: int | None = None,
    matrix: dict[str, Any] | None = None,
    authorities: dict[str, dict[str, str]] | None = None,
) -> tuple[dict[str, Any], tuple[dict[str, Any], ...]]:
    """Validate one signed release set and its exact five signed target manifests."""

    matrix = load_matrix() if matrix is None else matrix
    authorities = load_authority_registry() if authorities is None else authorities
    release_set = validate_envelope(
        release_set_envelope,
        retained_rollback_generation=retained_rollback_generation,
        matrix=matrix,
        authorities=authorities,
    )
    if release_set["contract"] != "ctx-managed-pair-release-set":
        raise ContractError("release bundle root must be a managed pair release set")

    references = release_set["target_manifests"]
    reference_keys: list[str] = []
    for reference in references:
        reference_keys.append(reference["manifest_object_key"])
    if (
        len(reference_keys) != len(TARGET_IDS)
        or len(set(reference_keys)) != len(TARGET_IDS)
        or not isinstance(manifest_envelopes_by_object_key, Mapping)
        or len(manifest_envelopes_by_object_key) != len(TARGET_IDS)
        or set(manifest_envelopes_by_object_key) != set(reference_keys)
    ):
        raise ContractError("release bundle must contain exactly the five referenced manifest object keys")

    manifests: list[dict[str, Any]] = []
    release_identity = {
        "channel": release_set["channel"],
        "release name": release_set["release_name"],
        "rollback generation": release_set["rollback_generation"],
        "snapshot fingerprint": release_set["snapshot"]["fingerprint"],
        "invocation compatibility fingerprint": release_set["compatibility"]["invocation_fingerprint"],
        "Core-capability compatibility fingerprint": release_set["compatibility"]["core_capability_fingerprint"],
        "release authority": release_set["release_authority_key_id"],
        "target matrix": release_set["target_matrix_sha256"],
    }
    for reference in references:
        object_key = reference["manifest_object_key"]
        envelope_bytes = manifest_envelopes_by_object_key[object_key]
        if not isinstance(envelope_bytes, bytes):
            raise ContractError("release bundle target manifest must be a byte string")
        if len(envelope_bytes) != reference["manifest_size_bytes"]:
            raise ContractError("release bundle target manifest size does not match its reference")
        digest = hashlib.sha256(envelope_bytes).hexdigest()
        if digest != reference["manifest_sha256"]:
            raise ContractError("release bundle target manifest hash does not match its reference")
        expected_object_key = f"sha256/{digest}/{reference['manifest_name']}"
        if object_key != expected_object_key:
            raise ContractError("release bundle target manifest object key does not match its bytes and name")
        try:
            envelope = json.loads(
                envelope_bytes.decode("utf-8"),
                object_pairs_hook=unique_object,
                parse_constant=lambda _: (_ for _ in ()).throw(ValueError()),
            )
        except ContractError:
            raise
        except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
            raise ContractError("release bundle target manifest is not a UTF-8 JSON envelope") from error
        if not isinstance(envelope, dict):
            raise ContractError("release bundle target manifest envelope must contain an object")
        manifest = validate_envelope(
            envelope,
            retained_rollback_generation=retained_rollback_generation,
            matrix=matrix,
            authorities=authorities,
        )
        if manifest["contract"] != "ctx-managed-pair-manifest":
            raise ContractError("release bundle target reference does not contain a managed pair manifest")
        if manifest["target"]["id"] != reference["target_id"]:
            raise ContractError("release bundle target manifest identity does not match its reference")
        manifest_identity = {
            "channel": manifest["channel"],
            "release name": manifest["release_name"],
            "rollback generation": manifest["rollback_generation"],
            "snapshot fingerprint": manifest["snapshot"]["fingerprint"],
            "invocation compatibility fingerprint": manifest["compatibility"]["invocation_fingerprint"],
            "Core-capability compatibility fingerprint": manifest["compatibility"]["core_capability_fingerprint"],
            "release authority": manifest["release_authority_key_id"],
            "target matrix": manifest["target_matrix_sha256"],
        }
        for label, expected in release_identity.items():
            if manifest_identity[label] != expected:
                raise ContractError(f"release bundle target manifest {label} does not match the release set")
        manifests.append(manifest)
    return release_set, tuple(manifests)


def main() -> int:
    try:
        if sys.argv[1:]:
            raise ContractError("usage: check-managed-pair-contracts.py")
        schemas = {}
        for path in (MANIFEST_SCHEMA_PATH, RELEASE_SET_SCHEMA_PATH, STATE_SCHEMA_PATH, ROOT / "contracts" / "ctx-managed-pair-signed-envelope-v1.schema.json"):
            schema = load_json(path)
            if schema.get("$schema") != "https://json-schema.org/draft/2020-12/schema":
                raise ContractError(f"{path.name} is not a JSON Schema 2020-12 contract")
            schemas[path] = schema
        validate_component_bound_authority(
            schemas[MANIFEST_SCHEMA_PATH], schemas[STATE_SCHEMA_PATH]
        )
        load_matrix()
        load_authority_registry()
    except ContractError as error:
        print(f"managed pair contracts: {error}", file=sys.stderr)
        return 1
    print("managed pair contracts: OK (schemas, authority registry, five-target matrix)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
