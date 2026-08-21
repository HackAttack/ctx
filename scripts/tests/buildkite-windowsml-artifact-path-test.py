#!/usr/bin/env python3
"""Cross-platform Buildkite contract for the Windows ML handoff artifacts."""

from __future__ import annotations

import copy
import json
from pathlib import Path
import re
import subprocess
import sys
import unittest


WINDOWS_ML_ASSET = "ctx-windowsml-windows-x64.zip"
WINDOWS_ONNX_ASSET = "ctx-onnxruntime-windows-x64.zip"
WINDOWS_ML_UPLOADS = (
    f"target/public-cli-artifacts/{WINDOWS_ML_ASSET}",
    f"target/public-cli-artifacts/{WINDOWS_ML_ASSET}.sha256",
    f"target/public-cli-artifacts/{WINDOWS_ML_ASSET}.asset.json",
)
WINDOWS_ONNX_UPLOADS = (
    f"target/public-cli-artifacts/{WINDOWS_ONNX_ASSET}",
    f"target/public-cli-artifacts/{WINDOWS_ONNX_ASSET}.sha256",
)
UPLOADS = WINDOWS_ONNX_UPLOADS + WINDOWS_ML_UPLOADS
WINDOWS_ML_UPLOADS_RENDERED = tuple(
    path.replace("/", "\\") for path in WINDOWS_ML_UPLOADS
)
WINDOWS_ONNX_UPLOADS_RENDERED = tuple(
    path.replace("/", "\\") for path in WINDOWS_ONNX_UPLOADS
)
WINDOWS_ML_SELECTOR = f"*{WINDOWS_ML_ASSET}*"
WINDOWS_ONNX_SELECTOR = f"*{WINDOWS_ONNX_ASSET}*"
LINUX_ONLY_SELECTOR = f"target/public-cli-artifacts/{WINDOWS_ML_ASSET}*"


class ContractError(ValueError):
    """Raised when the Windows ML Buildkite handoff is not portable."""


def load_pipeline(path: Path) -> dict[str, object]:
    encoded = subprocess.check_output(
        [
            "ruby",
            "-rjson",
            "-ryaml",
            "-e",
            "print JSON.generate(YAML.load_file(ARGV.fetch(0)))",
            str(path),
        ],
        text=True,
    )
    value = json.loads(encoded)
    if not isinstance(value, dict):
        raise ContractError("pipeline root must be a mapping")
    return value


def keyed_steps(value: dict[str, object]) -> dict[str, dict[str, object]]:
    steps = value.get("steps")
    if not isinstance(steps, list):
        raise ContractError("pipeline steps are missing")
    return {
        step["key"]: step
        for step in steps
        if isinstance(step, dict) and isinstance(step.get("key"), str)
    }


def buildkite_path_matches(pattern: str, path: str) -> bool:
    """Model Buildkite download matching, where * spans path separators."""
    expression = re.escape(pattern).replace(r"\*", ".*")
    return re.fullmatch(expression, path) is not None


def artifact_selector(command: str, step_key: str, label: str) -> str:
    downloads = re.findall(
        r"buildkite-agent artifact download\s+\\\s+"
        r'"([^"]+)"\s+\.\s+\\\s+--step\s+([^\s]+)',
        command,
    )
    selectors = [query for query, step in downloads if step == step_key]
    if len(selectors) != 1:
        raise ContractError(f"{label} must have one Windows artifact download")
    return selectors[0]


def validate_pipeline(value: dict[str, object]) -> None:
    keyed = keyed_steps(value)
    try:
        producer = keyed["semantic-runtime-windows-ml"]
        handoff = keyed["semantic-release-handoff"]
        github_release = keyed["github-release-assets"]
    except KeyError as error:
        raise ContractError(f"missing Buildkite step: {error.args[0]}") from error

    producer_agents = producer.get("agents")
    if not isinstance(producer_agents, dict) or producer_agents.get("os") != "windows":
        raise ContractError("Windows ML artifacts must originate on Windows")
    if producer.get("artifact_paths") != list(UPLOADS):
        raise ContractError("Windows runtime producer must upload the exact five slash-declared paths")

    handoff_agents = handoff.get("agents")
    if not isinstance(handoff_agents, dict) or handoff_agents.get("os") != "linux":
        raise ContractError("Semantic handoff must gather on Linux")
    command = handoff.get("command")
    if not isinstance(command, str):
        raise ContractError("Semantic handoff command is missing")
    selector = artifact_selector(
        command, "semantic-runtime-windows-ml", "Semantic handoff"
    )
    if selector != WINDOWS_ML_SELECTOR:
        raise ContractError("Windows ML handoff must use the portable whole-path selector")
    if not all(
        buildkite_path_matches(selector, path)
        for path in WINDOWS_ML_UPLOADS_RENDERED
    ):
        raise ContractError("Windows ML selector does not match Windows-rendered upload paths")

    linux_paths = tuple(path.replace("\\", "/") for path in WINDOWS_ML_UPLOADS_RENDERED)
    if linux_paths != WINDOWS_ML_UPLOADS:
        raise ContractError("Linux download normalization must reconstruct the staging paths")

    github_agents = github_release.get("agents")
    if not isinstance(github_agents, dict) or github_agents.get("os") != "linux":
        raise ContractError("GitHub release assembly must gather on Linux")
    github_command = github_release.get("command")
    if not isinstance(github_command, str):
        raise ContractError("GitHub release assembly command is missing")
    onnx_selector = artifact_selector(
        github_command, "semantic-runtime-windows-ml", "GitHub release assembly"
    )
    if onnx_selector != WINDOWS_ONNX_SELECTOR:
        raise ContractError("Windows ONNX handoff must use the portable whole-path selector")
    if not all(
        buildkite_path_matches(onnx_selector, path)
        for path in WINDOWS_ONNX_UPLOADS_RENDERED
    ):
        raise ContractError("Windows ONNX selector does not match Windows-rendered upload paths")


class WindowsMlArtifactPathTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.pipeline = load_pipeline(Path(sys.argv[1]))

    def test_windows_uploads_are_gathered_on_linux(self) -> None:
        validate_pipeline(self.pipeline)

    def test_linux_only_selector_mutation_is_rejected(self) -> None:
        self.assertFalse(
            any(
                buildkite_path_matches(LINUX_ONLY_SELECTOR, path)
                for path in WINDOWS_ML_UPLOADS_RENDERED
            )
        )
        mutated = copy.deepcopy(self.pipeline)
        handoff = keyed_steps(mutated)["semantic-release-handoff"]
        command = handoff["command"]
        self.assertIsInstance(command, str)
        handoff["command"] = command.replace(WINDOWS_ML_SELECTOR, LINUX_ONLY_SELECTOR)
        with self.assertRaisesRegex(ContractError, "portable whole-path selector"):
            validate_pipeline(mutated)

    def test_windows_upload_declaration_mutation_is_rejected(self) -> None:
        mutated = copy.deepcopy(self.pipeline)
        producer = keyed_steps(mutated)["semantic-runtime-windows-ml"]
        producer["artifact_paths"] = list(UPLOADS[1:])
        with self.assertRaisesRegex(ContractError, "exact five slash-declared paths"):
            validate_pipeline(mutated)


if __name__ == "__main__":
    unittest.main(argv=[sys.argv[0]])
