#!/usr/bin/env bash
set -euo pipefail

pipeline=".buildkite/pipeline.yml"
for required in \
  "${pipeline}" \
  scripts/buildkite-public-ci.sh \
  scripts/buildkite/download-linux-factory-artifacts.sh \
  scripts/release/build-public-candidate-on-linux.sh \
  scripts/validate-public-cli-factory-artifact.sh \
  scripts/stage-github-release-assets.sh \
  scripts/check-sdks.sh; do
  [[ -f "${required}" ]] || {
    printf 'Buildkite release input missing: %s\n' "${required}" >&2
    exit 1
  }
done

python3 - "${pipeline}" <<'PY'
from __future__ import annotations

import json
import re
import subprocess
import sys


def fail(message: str) -> None:
    raise SystemExit(f"Buildkite release pipeline: {message}")


try:
    encoded = subprocess.check_output(
        [
            "ruby",
            "-rjson",
            "-ryaml",
            "-e",
            "print JSON.generate(YAML.load_file(ARGV.fetch(0)))",
            sys.argv[1],
        ],
        text=True,
    )
except (OSError, subprocess.CalledProcessError) as error:
    fail("Ruby YAML parser is required for the pipeline contract")
value = json.loads(encoded)
steps = value.get("steps") if isinstance(value, dict) else None
if not isinstance(steps, list):
    fail("steps are missing")
keyed = {
    step.get("key"): step
    for step in steps
    if isinstance(step, dict) and isinstance(step.get("key"), str)
}
required = {
    "public-smoke",
    "public-nightly",
    "public-release",
    "sdk-swift-required",
    "public-cli-linux-factory",
    "public-cli-linux-x64-native-smoke",
    "public-cli-linux-aarch64-native-smoke",
    "public-cli-macos-arm64-native-smoke",
    "public-cli-macos-x64-runtime-producer",
    "public-cli-macos-x64-native-smoke",
    "public-cli-windows-x64-native-smoke",
    "github-release-candidate",
    "semantic-model-archives",
    "semantic-coreml-archive",
    "semantic-runtime-linux-cuda12",
    "semantic-runtime-windows-ml",
    "semantic-release-handoff",
}
if set(keyed) != required:
    fail(
        f"unexpected step keys: missing={sorted(required-set(keyed))} "
        f"extra={sorted(set(keyed)-required)}"
    )

for key, mode in (
    ("public-smoke", "ci"),
    ("public-nightly", "nightly"),
    ("public-release", "release"),
):
    if keyed[key].get("command", "").strip() != (
        f"bash scripts/buildkite-public-ci.sh --mode={mode}"
    ):
        fail(f"{key} does not own the exact {mode} validation route")

linux_x64_selector = {
    "queue": "release-linux-managed",
    "ctx-runner-class": "release-linux-control",
    "ctx-release-os": "ubuntu-24.04",
    "ctx-release-nested-docker": "true",
    "os": "linux",
    "arch": "x86_64",
}
public_release_selector = {
    **linux_x64_selector,
    "ctx-release-task-bind": "true",
    "ctx-release-unshare-clone-fs": "true",
}
linux_x64_keys = {
    "public-release",
    "public-cli-linux-factory",
    "public-cli-linux-x64-native-smoke",
    "github-release-candidate",
    "semantic-model-archives",
    "semantic-runtime-linux-cuda12",
    "semantic-release-handoff",
}
for key, step in keyed.items():
    agents = step.get("agents", {})
    if key == "public-release":
        if agents != public_release_selector:
            fail("public-release must require task-bind and exact CLONE_FS authority")
    elif key in linux_x64_keys:
        if agents != linux_x64_selector:
            fail(f"{key} must require the exact Linux x86_64 release authority selector")
    elif any(
        tag in agents
        for tag in (
            "ctx-release-os",
            "ctx-release-nested-docker",
            "ctx-release-task-bind",
            "ctx-release-unshare-clone-fs",
        )
    ):
        fail(f"{key} must not require Linux x86_64 release authority tags")

for key in ("public-cli-linux-x64-native-smoke", "public-cli-linux-aarch64-native-smoke"):
    if keyed[key].get("env") != {
        "CTX_PUBLIC_CLI_RUNTIME_AUTHORITY_BASELINE": "ubuntu-24.04"
    }:
        fail(f"{key} must require the Ubuntu 24.04 runtime authority")

factory = keyed["public-cli-linux-factory"]
factory_command = factory.get("command", "")
agents = factory.get("agents", {})
if agents != linux_x64_selector:
    fail("factory must use the managed Linux x86_64 release queue")
if (
    "build-public-candidate-on-linux.sh" not in factory_command
    or "--macos-sdk" not in factory_command
):
    fail("factory must invoke the one Linux construction entry point with an SDK")
if factory.get("secrets"):
    fail("factory must acquire Apple signing values only at the signing boundary")
if factory.get("artifact_paths") != ["target/public-cli-artifacts/*"]:
    fail("factory must upload its complete candidate directory")

native = {
    "public-cli-linux-x64-native-smoke": (
        "linux-x64", "release-linux-managed", "linux", "x86_64"
    ),
    "public-cli-linux-aarch64-native-smoke": (
        "linux-aarch64", "linux-arm64", "linux", "arm64"
    ),
    "public-cli-macos-arm64-native-smoke": (
        "macos-arm64", "ctx-release-macos-arm64", "darwin", "arm64"
    ),
    "public-cli-macos-x64-native-smoke": (
        "macos-x64", "ctx-mac-gui-shared-x64", "darwin", "x86_64"
    ),
    "public-cli-windows-x64-native-smoke": (
        "windows-x64", "windows-x64", "windows", "x86_64"
    ),
}
for key, (platform, queue, os_name, arch) in native.items():
    step = keyed[key]
    if key == "public-cli-macos-arm64-native-smoke":
        expected_dependency = ["public-cli-linux-factory", "semantic-coreml-archive"]
    elif key == "public-cli-macos-x64-native-smoke":
        expected_dependency = [
            "public-cli-linux-factory",
            "public-cli-macos-x64-runtime-producer",
        ]
    else:
        expected_dependency = "public-cli-linux-factory"
    if step.get("depends_on") != expected_dependency:
        fail(f"{key} has the wrong strict production dependencies")
    agents = step.get("agents", {})
    if (agents.get("queue"), agents.get("os"), agents.get("arch")) != (
        queue,
        os_name,
        arch,
    ):
        fail(f"{key} has the wrong native runner")
    command = step.get("command", "")
    if "download-linux-factory-artifacts.sh" not in command:
        fail(f"{key} must download factory artifacts")
    if "validate-public-cli-factory-artifact.sh" not in command or platform not in command:
        fail(f"{key} must run exact-byte native validation")
    if re.search(r"cargo (?:build|zigbuild)|bazelw run //:ctx_release", command):
        fail(f"{key} must never rebuild the candidate")

coreml_archive = (
    "target/public-cli-artifacts/"
    "ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz"
)
macos_arm_command = keyed["public-cli-macos-arm64-native-smoke"].get("command", "")
if (
    macos_arm_command.count(f'"{coreml_archive}*"') != 1
    or macos_arm_command.count("--step semantic-coreml-archive") != 1
    or macos_arm_command.count(f"--coreml-archive {coreml_archive}") != 1
):
    fail("macos-arm64 native smoke must download and bind the exact candidate Core ML set")
if "https://cli.ctx.rs" in macos_arm_command:
    fail("macos-arm64 native smoke must not fetch a published Core ML asset")

producer = keyed["public-cli-macos-x64-runtime-producer"]
if (
    "build-onnxruntime-sidecar.sh macos-x64" not in producer.get("command", "")
    or "stage-github-release-assets.sh --transcode-runtime macos-x64" not in producer.get("command", "")
    or producer.get("depends_on") is not None
):
    fail("macos-x64 runtime producer must own only runtime construction")
if producer.get("secrets"):
    fail("macos-x64 producer must acquire Apple values only at signing")
if "download-linux-factory-artifacts.sh" in producer.get("command", ""):
    fail("macos-x64 runtime producer must not consume factory CLI artifacts")
if producer.get("artifact_paths") != [
    "target/public-cli-artifacts/ctx-onnxruntime-macos-x64*"
]:
    fail("macos-x64 runtime producer must upload only its exact runtime artifact set")
if "build-onnxruntime-sidecar.sh macos-x64" in keyed[
    "public-cli-macos-x64-native-smoke"
].get("command", ""):
    fail("macos-x64 native lane must not source-build its runtime")
handoff = keyed["semantic-release-handoff"]
if "public-cli-macos-x64-runtime-producer" not in handoff.get("depends_on", []):
    fail("Semantic handoff must reuse the macos-x64 native runtime")

candidate = keyed["github-release-candidate"]
expected_dependencies = [
    "public-release",
    "sdk-swift-required",
    "public-cli-linux-factory",
    "public-cli-linux-x64-native-smoke",
    "public-cli-linux-aarch64-native-smoke",
    "public-cli-macos-arm64-native-smoke",
    "public-cli-macos-x64-runtime-producer",
    "public-cli-macos-x64-native-smoke",
    "public-cli-windows-x64-native-smoke",
]
if candidate.get("depends_on") != expected_dependencies:
    fail("candidate staging has the wrong strict dependency set")
if candidate.get("allow_dependency_failure") or candidate.get("soft_fail"):
    fail("candidate staging must fail closed")
candidate_command = candidate.get("command", "")
if (
    'download-linux-factory-artifacts.sh "*"' not in candidate_command
    or "stage-github-release-assets.sh" not in candidate_command
    or "CTX_PUBLIC_RELEASE_SOURCE_COMMIT" not in candidate_command
):
    fail("candidate staging must consume the complete factory output and bind HEAD")
for proof in (
    "linux-x64",
    "linux-aarch64",
    "macos-arm64",
    "macos-x64",
    "windows-x64",
):
    if f"ctx-{proof}.native-execution.json" not in candidate_command:
        fail(f"candidate staging must consume native {proof} proof")

for step in steps:
    if not isinstance(step, dict):
        continue
    command = str(step.get("command", ""))
    match = re.search(
        r"(?<![$])[$](?:[{][^}\n]+[}]|[A-Za-z_][A-Za-z0-9_]*)", command
    )
    if match:
        fail(f"{step.get('key')} exposes {match.group(0)} to Buildkite interpolation")

print(
    "Buildkite release pipeline: one Linux factory, five exact-byte native "
    "validators, strict staging"
)
PY

python3 scripts/tests/buildkite-windowsml-artifact-path-test.py "${pipeline}"
bash scripts/tests/buildkite-public-ci-cache-test.sh
bash scripts/tests/check-sdks-required-groups-test.sh
python3 scripts/check-sdk-ci-pipeline.py \
  "${pipeline}" scripts/buildkite-public-ci.sh scripts/check-sdks.sh
