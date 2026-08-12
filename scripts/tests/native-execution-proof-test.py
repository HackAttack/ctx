#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
from pathlib import Path
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "native-execution-proof.py"
spec = importlib.util.spec_from_file_location("native_execution_proof", MODULE_PATH)
assert spec is not None and spec.loader is not None
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class NativeExecutionProofTest(unittest.TestCase):
    def test_proof_binds_exact_artifact_and_passed_smoke(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifact = root / "ctx"
            smoke = root / "candidate-smoke.json"
            proof = root / "ctx-linux-x64.native-execution.json"
            artifact.write_bytes(b"candidate bytes")
            smoke.write_text(
                '{"kind":"ctx-native-candidate-smoke","schema_version":1,"status":"passed"}\n',
                encoding="utf-8",
            )
            module.create("linux-x64", artifact, smoke, proof)
            module.verify("linux-x64", artifact, proof)
            artifact.write_bytes(b"changed candidate bytes")
            with self.assertRaisesRegex(ValueError, "different artifact"):
                module.verify("linux-x64", artifact, proof)

    def test_failed_smoke_cannot_create_proof(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifact = root / "ctx"
            smoke = root / "candidate-smoke.json"
            artifact.write_bytes(b"candidate bytes")
            smoke.write_text(
                '{"kind":"ctx-native-candidate-smoke","schema_version":1,"status":"failed"}\n',
                encoding="utf-8",
            )
            with self.assertRaisesRegex(ValueError, "not passed"):
                module.create("linux-x64", artifact, smoke, root / "proof.json")


if __name__ == "__main__":
    unittest.main()
