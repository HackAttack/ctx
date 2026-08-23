#!/usr/bin/env bash
set -euo pipefail
umask 077

usage() {
  cat >&2 <<'USAGE'
Usage:
  write-semantic-execution-receipt.sh \
    OUTPUT PLATFORM \
    CLI_NAME CLI_SHA256 \
    ARCHIVE_NAME ARCHIVE_SHA256 \
    RUNTIME_NAME RUNTIME_SHA256 \
    HOST_SYSTEM HOST_ARCH HOST_NATIVE_ARCH PROCESS_TRANSLATED \
    NATIVE_ARCH_PROBE HARDWARE_IDENTITY EMULATION HYPERVISOR \
    EVIDENCE_COMPLETE RUNNER_ID AUTHORITY MODEL_KEY \
    SOURCE_COMMIT BUILD_ID BUILD_NUMBER JOB_ID RETRY_COUNT STEP_KEY

Construct an atomic schema-v2 receipt for authoritative native semantic
execution. Arguments are ordered to mirror the receipt's artifact, host,
semantic, and Buildkite provenance sections.
USAGE
}

if (($# != 26)); then
  usage
  exit 2
fi

python3 -I - "$@" <<'PY'
import json
import os
import re
import sys
from pathlib import Path

(
    output_text,
    platform,
    cli_name,
    cli_sha256,
    archive_name,
    archive_sha256,
    runtime_name,
    runtime_sha256,
    host_system,
    host_arch,
    host_native_arch,
    process_translated_text,
    native_arch_probe,
    hardware_identity,
    emulation,
    hypervisor,
    evidence_complete_text,
    runner_id,
    authority,
    model_key,
    source_commit,
    build_id,
    build_number_text,
    job_id,
    retry_count_text,
    step_key,
) = sys.argv[1:]

if authority != "authoritative":
    raise SystemExit("semantic receipt requires authoritative native execution")
if process_translated_text != "0" or evidence_complete_text != "1":
    raise SystemExit("semantic receipt host evidence is incomplete or translated")
for label, value in (
    ("CLI", cli_sha256),
    ("runtime archive", archive_sha256),
    ("nested runtime", runtime_sha256),
):
    if re.fullmatch(r"[0-9a-f]{64}", value) is None:
        raise SystemExit(f"semantic receipt {label} digest is malformed")
if re.fullmatch(r"[0-9a-f]{40}", source_commit) is None or source_commit == "0" * 40:
    raise SystemExit("semantic receipt source commit is malformed")

document = {
    "authority": authority,
    "backend": "onnxruntime-cpu",
    "cli_artifact": {"name": cli_name, "sha256": cli_sha256},
    "host_evidence": {
        "arch": host_arch,
        "emulation": emulation,
        "evidence_complete": True,
        "hardware_identity": hardware_identity,
        "hypervisor": hypervisor,
        "native_arch": host_native_arch,
        "native_arch_probe": native_arch_probe,
        "process_translated": 0,
        "runner_id": runner_id,
        "system": host_system,
    },
    "kind": "ctx-semantic-native-execution",
    "platform": platform,
    "provenance": {
        "build_id": build_id,
        "build_number": int(build_number_text),
        "job_id": job_id,
        "retry_count": int(retry_count_text),
        "source_commit": source_commit,
        "step_key": step_key,
    },
    "runtime_archive": {
        "name": archive_name,
        "nested_artifact_name": f"lib/{runtime_name}",
        "nested_artifact_sha256": runtime_sha256,
        "sha256": archive_sha256,
    },
    "schema_version": 2,
    "semantic": {
        "effective_mode": "semantic",
        "indexed_chunks_minimum": 1,
        "model_key": model_key,
        "requested_mode": "semantic",
        "status": "ready",
    },
    "status": "passed",
}
output = Path(output_text)
temporary = output.with_name(f".{output.name}.tmp.{os.getpid()}")
try:
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o644)
    with os.fdopen(descriptor, "w", encoding="utf-8", newline="\n") as stream:
        json.dump(document, stream, sort_keys=True, separators=(",", ":"))
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    if output.exists() or output.is_symlink():
        raise SystemExit(f"semantic smoke receipt already exists: {output}")
    os.replace(temporary, output)
finally:
    temporary.unlink(missing_ok=True)
PY
