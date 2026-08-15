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


ASSET = "ctx-windowsml-windows-x64.zip"
UPLOADS = (
    f"target/public-cli-artifacts/{ASSET}",
    f"target/public-cli-artifacts/{ASSET}.sha256",
    f"target/public-cli-artifacts/{ASSET}.asset.json",
)
WINDOWS_UPLOADS = tuple(path.replace("/", "\\") for path in UPLOADS)
PORTABLE_SELECTOR = f"*{ASSET}*"
LINUX_ONLY_SELECTOR = f"target/public-cli-artifacts/{ASSET}*"


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


def windows_ml_selector(command: str) -> str:
    downloads = re.findall(
        r"buildkite-agent artifact download\s+\\\s+"
        r'"([^"]+)"\s+\.\s+\\\s+--step\s+([^\s]+)',
        command,
    )
    selectors = [query for query, step in downloads if step == "semantic-runtime-windows-ml"]
    if len(selectors) != 1:
        raise ContractError("handoff must have one Windows ML artifact download")
    return selectors[0]


def validate_pipeline(value: dict[str, object]) -> None:
    keyed = keyed_steps(value)
    try:
        producer = keyed["semantic-runtime-windows-ml"]
        handoff = keyed["semantic-release-handoff"]
    except KeyError as error:
        raise ContractError(f"missing Buildkite step: {error.args[0]}") from error

    producer_agents = producer.get("agents")
    if not isinstance(producer_agents, dict) or producer_agents.get("os") != "windows":
        raise ContractError("Windows ML artifacts must originate on Windows")
    if producer.get("artifact_paths") != list(UPLOADS):
        raise ContractError("Windows ML producer must upload the exact three slash-declared paths")

    handoff_agents = handoff.get("agents")
    if not isinstance(handoff_agents, dict) or handoff_agents.get("os") != "linux":
        raise ContractError("Semantic handoff must gather on Linux")
    command = handoff.get("command")
    if not isinstance(command, str):
        raise ContractError("Semantic handoff command is missing")
    selector = windows_ml_selector(command)
    if selector != PORTABLE_SELECTOR:
        raise ContractError("Windows ML handoff must use the portable whole-path selector")
    if not all(buildkite_path_matches(selector, path) for path in WINDOWS_UPLOADS):
        raise ContractError("Windows ML selector does not match Windows-rendered upload paths")

    linux_paths = tuple(path.replace("\\", "/") for path in WINDOWS_UPLOADS)
    if linux_paths != UPLOADS:
        raise ContractError("Linux download normalization must reconstruct the staging paths")


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
                for path in WINDOWS_UPLOADS
            )
        )
        mutated = copy.deepcopy(self.pipeline)
        handoff = keyed_steps(mutated)["semantic-release-handoff"]
        command = handoff["command"]
        self.assertIsInstance(command, str)
        handoff["command"] = command.replace(PORTABLE_SELECTOR, LINUX_ONLY_SELECTOR)
        with self.assertRaisesRegex(ContractError, "portable whole-path selector"):
            validate_pipeline(mutated)

    def test_windows_upload_declaration_mutation_is_rejected(self) -> None:
        mutated = copy.deepcopy(self.pipeline)
        producer = keyed_steps(mutated)["semantic-runtime-windows-ml"]
        producer["artifact_paths"] = list(UPLOADS[1:])
        with self.assertRaisesRegex(ContractError, "exact three slash-declared paths"):
            validate_pipeline(mutated)


if __name__ == "__main__":
    unittest.main(argv=[sys.argv[0]])
