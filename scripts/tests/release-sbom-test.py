#!/usr/bin/env python3

from __future__ import annotations

import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
import zipfile

_TEST_DIRECTORY = str(Path(__file__).resolve().parent)
sys.path.insert(0, _TEST_DIRECTORY)
try:
    from release_sbom_test_lock import (
        SYNTHETIC_WORKSPACE_VERSION,
        package,
        synthetic_lock_text,
    )
finally:
    del sys.path[0]
del _TEST_DIRECTORY

SCRIPT = Path(__file__).resolve().parents[1] / "release-sbom.py"
SCHEMA = (
    Path(__file__).resolve().parents[2]
    / "contracts"
    / "release-candidate-manifest-v1.schema.json"
)
COMMIT = "0123456789abcdef0123456789abcdef01234567"
CRATE_REPOSITORY_PREFIX = "rules_rust++crate+"
TANTIVY_FEATURES = (
    "columnar-zstd-compression",
    "fs4",
    "lz4-compression",
    "lz4_flex",
    "memmap2",
    "mmap",
    "tempfile",
    "zstd",
    "zstd-compression",
)
WORKSPACE_PACKAGES = (
    ("ctx", "crates/ctx-cli"),
    ("ctx-agent-application", "crates/ctx-agent-application"),
    ("ctx-agent-integrations", "crates/ctx-agent-integrations"),
    ("ctx-companion-bridge", "crates/ctx-companion-bridge"),
    ("ctx-cli-presentation", "crates/ctx-cli-presentation"),
    ("ctx-client-observability", "crates/ctx-client-observability"),
    ("ctx-daemon-application", "crates/ctx-daemon-application"),
    ("ctx-daemon-cli", "crates/ctx-daemon-cli"),
    ("ctx-daemon-runtime", "crates/ctx-daemon-runtime"),
    ("ctx-daemon-service", "crates/ctx-daemon-service"),
    ("ctx-history-capture", "crates/ctx-history-capture"),
    ("ctx-history-capture-composition", "crates/ctx-history-capture-composition"),
    ("ctx-history-capture-model", "crates/ctx-history-capture-model"),
    ("ctx-history-cli", "crates/ctx-history-cli"),
    ("ctx-history-capture-runtime", "crates/ctx-history-capture-runtime"),
    ("ctx-history-core", "crates/ctx-history-core"),
    ("ctx-history-index-format", "crates/ctx-history-index-format"),
    ("ctx-history-index", "crates/ctx-history-index"),
    ("ctx-history-index-query", "crates/ctx-history-index-query"),
    ("ctx-history-jsonl", "crates/ctx-history-jsonl"),
    ("ctx-history-platform", "crates/ctx-history-platform"),
    (
        "ctx-history-provider-claude-cursor",
        "crates/ctx-history-provider-claude-cursor",
    ),
    (
        "ctx-history-provider-docproj",
        "crates/ctx-history-provider-docproj",
    ),
    ("ctx-history-provider-gemini", "crates/ctx-history-provider-gemini"),
    (
        "ctx-history-provider-mistral-mux",
        "crates/ctx-history-provider-mistral-mux",
    ),
    (
        "ctx-history-provider-native-jsonl",
        "crates/ctx-history-provider-native-jsonl",
    ),
    ("ctx-history-provider-runtime", "crates/ctx-history-provider-runtime"),
    ("ctx-history-provider-codex", "crates/ctx-history-provider-codex"),
    ("ctx-history-provider-trae", "crates/ctx-history-provider-trae"),
    (
        "ctx-history-providers-sqlite-selected",
        "crates/ctx-history-providers-sqlite-selected",
    ),
    (
        "ctx-history-providers-sqlite-inventory",
        "crates/ctx-history-providers-sqlite-inventory",
    ),
    (
        "ctx-history-providers-sqlite-logical",
        "crates/ctx-history-providers-sqlite-logical",
    ),
    (
        "ctx-history-providers-task-docs",
        "crates/ctx-history-providers-task-docs",
    ),
    ("ctx-history-source-io", "crates/ctx-history-source-io"),
    ("ctx-history-source-discovery", "crates/ctx-history-source-discovery"),
    ("ctx-history-source-sqlite", "crates/ctx-history-source-sqlite"),
    ("ctx-history-refresh", "crates/ctx-history-refresh"),
    (
        "ctx-history-providers-jsonl-shared",
        "crates/ctx-history-providers-jsonl-shared",
    ),
    ("ctx-history-refresh-execution", "crates/ctx-history-refresh-execution"),
    ("ctx-history-read-application", "crates/ctx-history-read-application"),
    ("ctx-managed-pair-engine", "crates/ctx-managed-pair-engine"),
    ("ctx-semantic-index", "crates/ctx-semantic-index"),
    ("ctx-semantic-model", "crates/ctx-semantic-model"),
    ("ctx-terminal", "crates/ctx-terminal"),
    ("ctx-upgrade-engine", "crates/ctx-upgrade-engine"),
)
EXTERNAL_PACKAGES = (
    ("base64", "0.22.0"),
    ("chrono", "0.4.0"),
    ("fs4", "0.1.0"),
    ("libc", "0.2.0"),
    ("lz4_flex", "0.11.0"),
    ("memmap2", "0.9.0"),
    ("regex", "1.0.0"),
    ("rusqlite", "0.32.1"),
    ("serde", "1.0.0"),
    ("serde_json", "1.0.0"),
    ("sha2", "0.10.9"),
    ("tantivy", "0.26.1"),
    ("tempfile", "3.0.0"),
    ("thiserror", "1.0.0"),
    ("uuid", "1.0.0"),
    ("zstd", "0.13.0"),
)
DOCUMENT_PROJECTION_DIRECT_DEPENDENCIES = {
    "chrono",
    "ctx-history-capture-model",
    "ctx-history-capture-runtime",
    "ctx-history-core",
    "ctx-history-provider-runtime",
    "ctx-history-source-discovery",
    "ctx-history-source-io",
    "ctx-history-source-sqlite",
    "rusqlite",
    "serde",
    "serde_json",
    "sha2",
    "thiserror",
}
LEGACY_RELEASE_ASSETS = (
    "ctx-linux-x64",
    "ctx-linux-x64.cdx.json",
    "ctx-linux-x64.third-party-notices.txt",
    "ctx-linux-aarch64",
    "ctx-linux-aarch64.cdx.json",
    "ctx-linux-aarch64.third-party-notices.txt",
    "ctx-macos-arm64",
    "ctx-macos-arm64.cdx.json",
    "ctx-macos-arm64.third-party-notices.txt",
    "ctx-macos-x64",
    "ctx-macos-x64.cdx.json",
    "ctx-macos-x64.third-party-notices.txt",
    "ctx-windows-x64.exe",
    "ctx-windows-x64.exe.cdx.json",
    "ctx-windows-x64.exe.third-party-notices.txt",
    "ctx-onnxruntime-linux-x64.tar.gz",
    "ctx-onnxruntime-linux-aarch64.tar.gz",
    "ctx-onnxruntime-macos-arm64.tar.gz",
    "ctx-onnxruntime-macos-x64.tar.gz",
    "ctx-onnxruntime-windows-x64.zip",
)
WINDOWS_RUNTIME_FILES = (
    "LICENSE",
    "ThirdPartyNotices.txt",
    "VERSION_NUMBER",
    "GIT_COMMIT_ID",
    "MICROSOFT_VC_RUNTIME_LICENSE.rtf",
    "lib/onnxruntime.dll",
    "lib/msvcp140.dll",
    "lib/msvcp140_1.dll",
    "lib/vcruntime140.dll",
    "lib/vcruntime140_1.dll",
)
RELEASE_AUTHORITY_CANDIDATES = (
    "ctx.candidate.json",
    "ctx-linux-aarch64.candidate.json",
    "ctx-macos-arm64.candidate.json",
    "ctx-macos-x64.candidate.json",
    "ctx.exe.candidate.json",
)


class ReleaseSbomTest(unittest.TestCase):
    package = staticmethod(package)

    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.runfiles = self.root / "runfiles"
        self.main_runfiles = self.runfiles / "_main"
        self.main_runfiles.mkdir(parents=True)
        self.target_id = "linux-x64"
        self.platform = "linux-x64"

        self.artifact = self.root / "ctx"
        self.artifact.write_bytes(b"exact release artifact\n")
        self.cargo_lock = self.root / "Cargo.lock"
        self.cargo_lock.write_text(synthetic_lock_text(), encoding="utf-8")
        self.module_file = self.root / "MODULE.bazel"
        self.module_file.write_text('module(name = "ctx")\n', encoding="utf-8")
        self.module_lock = self.root / "MODULE.bazel.lock"
        self.module_lock.write_text('{"lockFileVersion":21}\n', encoding="utf-8")
        self.candidate_schema = SCHEMA
        self.target_matrix = self.root / "release-targets-v1.json"
        self.target_matrix.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "targets": [
                        {
                            "id": "linux-x64",
                            "public_rust_target": "x86_64-unknown-linux-gnu",
                            "public_construction_authority": "linux-cross-cargo-zigbuild-v1",
                            "public_construction_label": "scripts/release/build-public-candidate-on-linux.sh",
                        }
                    ],
                },
                sort_keys=True,
                separators=(",", ":"),
            )
            + "\n",
            encoding="utf-8",
        )

        self.workspace_manifest = self.main_runfiles / "Cargo.toml"
        self.workspace_manifest.write_text(
            """\
[workspace]
members = [
  "crates/ctx-cli",
  "crates/ctx-agent-application",
  "crates/ctx-agent-integrations",
  "crates/ctx-companion-bridge",
  "crates/ctx-cli-presentation",
  "crates/ctx-client-observability",
  "crates/ctx-daemon-application",
  "crates/ctx-daemon-cli",
  "crates/ctx-daemon-runtime",
  "crates/ctx-daemon-service",
  "crates/ctx-history-capture",
  "crates/ctx-history-capture-composition",
  "crates/ctx-history-capture-model",
  "crates/ctx-history-cli",
  "crates/ctx-history-capture-runtime",
  "crates/ctx-history-core",
  "crates/ctx-history-index-format",
  "crates/ctx-history-index",
  "crates/ctx-history-index-query",
  "crates/ctx-history-jsonl",
  "crates/ctx-history-platform",
  "crates/ctx-history-provider-claude-cursor",
  "crates/ctx-history-provider-docproj",
  "crates/ctx-history-provider-gemini",
  "crates/ctx-history-provider-mistral-mux",
  "crates/ctx-history-provider-native-jsonl",
  "crates/ctx-history-provider-runtime",
  "crates/ctx-history-provider-codex",
  "crates/ctx-history-provider-trae",
  "crates/ctx-history-providers-sqlite-selected",
  "crates/ctx-history-providers-sqlite-inventory",
  "crates/ctx-history-providers-sqlite-logical",
  "crates/ctx-history-providers-task-docs",
  "crates/ctx-history-source-io",
  "crates/ctx-history-source-discovery",
  "crates/ctx-history-source-sqlite",
  "crates/ctx-history-refresh",
  "crates/ctx-history-providers-jsonl-shared",
  "crates/ctx-history-refresh-execution",
  "crates/ctx-history-read-application",
  "crates/ctx-managed-pair-engine",
  "crates/ctx-semantic-index",
  "crates/ctx-semantic-model",
  "crates/ctx-terminal",
  "crates/ctx-upgrade-engine",
]

[workspace.package]
version = "0.26.0"
license = "MIT"
repository = "https://github.com/ctxrs/ctx"

[workspace.dependencies]
chrono = { version = "0.4.0", default-features = false, features = ["std", "serde"] }
base64 = "0.22.0"
libc = "0.2.0"
regex = "1.0.0"
rusqlite = "0.32.1"
serde = { version = "1.0.0", features = ["derive", "rc"] }
serde_json = { version = "1.0.0", features = ["raw_value"] }
sha2 = "0.10.9"
tempfile = "3.0.0"
tantivy = { version = "0.26.1", default-features = false, features = ["mmap", "lz4-compression", "zstd-compression", "columnar-zstd-compression"] }
thiserror = "1.0.0"
uuid = "1.0.0"
""",
            encoding="utf-8",
        )
        (self.main_runfiles / "LICENSE").write_text(
            "Synthetic workspace MIT license.\n", encoding="utf-8"
        )
        for name, directory in WORKSPACE_PACKAGES:
            manifest = self.main_runfiles / directory / "Cargo.toml"
            manifest.parent.mkdir(parents=True)
            dependencies = {
                "ctx": (
                    "ctx-agent-application = { path = \"../ctx-agent-application\" }\n"
                    "ctx-agent-integrations = { path = \"../ctx-agent-integrations\" }\n"
                    "ctx-companion-bridge = { path = \"../ctx-companion-bridge\" }\n"
                    "ctx-cli-presentation = { path = \"../ctx-cli-presentation\" }\n"
                    "ctx-client-observability = { path = \"../ctx-client-observability\" }\n"
                    "ctx-daemon-cli = { path = \"../ctx-daemon-cli\" }\n"
                    "ctx-history-capture = { path = \"../ctx-history-capture\" }\n"
                    "ctx-history-capture-model = { path = \"../ctx-history-capture-model\" }\n"
                    "ctx-history-cli = { path = \"../ctx-history-cli\" }\n"
                    "ctx-history-jsonl = { path = \"../ctx-history-jsonl\" }\n"
                    "ctx-history-provider-codex = { path = \"../ctx-history-provider-codex\" }\n"
                    "ctx-history-refresh = { path = \"../ctx-history-refresh\" }\n"
                    "ctx-history-provider-docproj = { path = \"../ctx-history-provider-docproj\" }\n"
                    "ctx-history-provider-gemini = { path = \"../ctx-history-provider-gemini\" }\n"
                    "ctx-history-provider-native-jsonl = { path = \"../ctx-history-provider-native-jsonl\" }\n"
                    "ctx-history-provider-runtime = { path = \"../ctx-history-provider-runtime\" }\n"
                    "ctx-history-provider-trae = { path = \"../ctx-history-provider-trae\" }\n"
                    "ctx-history-providers-sqlite-selected = { path = \"../ctx-history-providers-sqlite-selected\" }\n"
                    "ctx-history-providers-sqlite-inventory = { path = \"../ctx-history-providers-sqlite-inventory\" }\n"
                    "ctx-history-providers-sqlite-logical = { path = \"../ctx-history-providers-sqlite-logical\" }\n"
                    "ctx-history-providers-jsonl-shared = { path = \"../ctx-history-providers-jsonl-shared\" }\n"
                    "ctx-history-providers-task-docs = { path = \"../ctx-history-providers-task-docs\" }\n"
                    "ctx-history-provider-mistral-mux = { path = \"../ctx-history-provider-mistral-mux\" }\n"
                    "ctx-history-source-io = { path = \"../ctx-history-source-io\" }\n"
                    "ctx-history-refresh-execution = { path = \"../ctx-history-refresh-execution\" }\n"
                    "ctx-history-read-application = { path = \"../ctx-history-read-application\" }\n"
                    "ctx-terminal = { path = \"../ctx-terminal\" }\n"
                    "ctx-upgrade-engine = { path = \"../ctx-upgrade-engine\" }"
                ),
                "ctx-agent-integrations": (
                    "ctx-history-core = { path = \"../ctx-history-core\" }"
                ),
                "ctx-companion-bridge": (
                    "ctx-history-platform = { path = \"../ctx-history-platform\" }"
                ),
                "ctx-agent-application": (
                    "ctx-agent-integrations = { path = \"../ctx-agent-integrations\" }\n"
                    "ctx-client-observability = { path = \"../ctx-client-observability\" }"
                ),
                "ctx-cli-presentation": (
                    "ctx-agent-application = { path = \"../ctx-agent-application\" }\n"
                    "ctx-agent-integrations = { path = \"../ctx-agent-integrations\" }\n"
                    "ctx-client-observability = { path = \"../ctx-client-observability\" }\n"
                    "ctx-history-cli = { path = \"../ctx-history-cli\" }\n"
                    "ctx-history-core = { path = \"../ctx-history-core\" }\n"
                    "ctx-history-read-application = { path = \"../ctx-history-read-application\" }\n"
                    "ctx-history-refresh = { path = \"../ctx-history-refresh\" }\n"
                    "ctx-terminal = { path = \"../ctx-terminal\" }\n"
                    "ctx-upgrade-engine = { path = \"../ctx-upgrade-engine\" }"
                ),
                "ctx-client-observability": (
                    "ctx-history-core = { path = \"../ctx-history-core\" }"
                ),
                "ctx-daemon-application": (
                    "ctx-client-observability = { path = \"../ctx-client-observability\" }\n"
                    "ctx-daemon-runtime = { path = \"../ctx-daemon-runtime\" }\n"
                    "ctx-daemon-service = { path = \"../ctx-daemon-service\" }\n"
                    "ctx-history-core = { path = \"../ctx-history-core\" }"
                ),
                "ctx-daemon-cli": (
                    "ctx-client-observability = { path = \"../ctx-client-observability\" }\n"
                    "ctx-daemon-application = { path = \"../ctx-daemon-application\" }\n"
                    "ctx-daemon-runtime = { path = \"../ctx-daemon-runtime\" }\n"
                    "ctx-daemon-service = { path = \"../ctx-daemon-service\" }\n"
                    "ctx-history-core = { path = \"../ctx-history-core\" }\n"
                    "ctx-history-index = { path = \"../ctx-history-index\" }\n"
                    "ctx-history-read-application = { path = \"../ctx-history-read-application\" }\n"
                    "ctx-semantic-index = { path = \"../ctx-semantic-index\" }\n"
                    "ctx-semantic-model = { path = \"../ctx-semantic-model\" }\n"
                    "ctx-terminal = { path = \"../ctx-terminal\" }\n"
                    "ctx-upgrade-engine = { path = \"../ctx-upgrade-engine\" }"
                ),
                "ctx-daemon-runtime": (
                    "ctx-history-core = { path = \"../ctx-history-core\" }"
                ),
                "ctx-history-capture": (
                    "ctx-history-capture-composition = { path = \"../ctx-history-capture-composition\" }\n"
                    "ctx-history-capture-model = { path = \"../ctx-history-capture-model\" }\n"
                    "ctx-history-capture-runtime = { path = \"../ctx-history-capture-runtime\" }\n"
                    "ctx-history-core = { path = \"../ctx-history-core\" }\n"
                    "ctx-history-jsonl = { path = \"../ctx-history-jsonl\" }\n"
                    "ctx-history-provider-claude-cursor = { path = \"../ctx-history-provider-claude-cursor\" }\n"
                    "ctx-history-provider-runtime = { path = \"../ctx-history-provider-runtime\" }"
                ),
                "ctx-history-capture-composition": (
                    "chrono.workspace = true\n"
                    "ctx-history-capture-model = { path = \"../ctx-history-capture-model\" }\n"
                    "ctx-history-capture-runtime = { path = \"../ctx-history-capture-runtime\" }\n"
                    "ctx-history-core = { path = \"../ctx-history-core\" }\n"
                    "ctx-history-index = { path = \"../ctx-history-index\" }\n"
                    "ctx-history-jsonl = { path = \"../ctx-history-jsonl\" }\n"
                    "ctx-history-provider-claude-cursor = { path = \"../ctx-history-provider-claude-cursor\" }\n"
                    "ctx-history-provider-codex = { path = \"../ctx-history-provider-codex\" }\n"
                    "ctx-history-provider-docproj = { path = \"../ctx-history-provider-docproj\" }\n"
                    "ctx-history-provider-gemini = { path = \"../ctx-history-provider-gemini\" }\n"
                    "ctx-history-provider-mistral-mux = { path = \"../ctx-history-provider-mistral-mux\" }\n"
                    "ctx-history-provider-native-jsonl = { path = \"../ctx-history-provider-native-jsonl\" }\n"
                    "ctx-history-provider-runtime = { path = \"../ctx-history-provider-runtime\" }\n"
                    "ctx-history-provider-trae = { path = \"../ctx-history-provider-trae\" }\n"
                    "ctx-history-providers-jsonl-shared = { path = \"../ctx-history-providers-jsonl-shared\" }\n"
                    "ctx-history-providers-sqlite-inventory = { path = \"../ctx-history-providers-sqlite-inventory\" }\n"
                    "ctx-history-providers-sqlite-logical = { path = \"../ctx-history-providers-sqlite-logical\" }\n"
                    "ctx-history-providers-sqlite-selected = { path = \"../ctx-history-providers-sqlite-selected\" }\n"
                    "ctx-history-providers-task-docs = { path = \"../ctx-history-providers-task-docs\" }\n"
                    "ctx-history-source-discovery = { path = \"../ctx-history-source-discovery\" }\n"
                    "ctx-history-source-io = { path = \"../ctx-history-source-io\" }\n"
                    "serde_json.workspace = true\n"
                    "sha2.workspace = true\n"
                    "tempfile.workspace = true\n"
                    "thiserror.workspace = true\n"
                    "uuid.workspace = true"
                ),
                "ctx-history-index": (
                    "ctx-history-index-format = { path = \"../ctx-history-index-format\" }\n"
                    "ctx-history-index-query = { path = \"../ctx-history-index-query\" }\n"
                    "ctx-semantic-model = { path = \"../ctx-semantic-model\" }\n"
                    "tantivy.workspace = true"
                ),
                "ctx-history-index-format": "tantivy.workspace = true",
                "ctx-history-index-query": (
                    "ctx-history-index-format = { path = \"../ctx-history-index-format\" }\n"
                    "tantivy.workspace = true"
                ),
                "ctx-history-jsonl": (
                    "ctx-history-capture-model = { path = \"../ctx-history-capture-model\" }\n"
                    "ctx-history-capture-runtime = { path = \"../ctx-history-capture-runtime\" }\n"
                    "ctx-history-core = { path = \"../ctx-history-core\" }\n"
                    "serde_json.workspace = true"
                ),
                "ctx-history-provider-claude-cursor": (
                    "ctx-history-capture-model = { path = \"../ctx-history-capture-model\" }\n"
                    "ctx-history-capture-runtime = { path = \"../ctx-history-capture-runtime\" }\n"
                    "ctx-history-core = { path = \"../ctx-history-core\" }\n"
                    "ctx-history-jsonl = { path = \"../ctx-history-jsonl\" }\n"
                    "ctx-history-provider-runtime = { path = \"../ctx-history-provider-runtime\" }"
                ),
                "ctx-history-provider-gemini": (
                    "ctx-history-capture-model = { path = \"../ctx-history-capture-model\" }\n"
                    "ctx-history-core = { path = \"../ctx-history-core\" }\n"
                    "ctx-history-jsonl = { path = \"../ctx-history-jsonl\" }\n"
                    "ctx-history-source-io = { path = \"../ctx-history-source-io\" }\n"
                    "chrono.workspace = true\n"
                    "serde.workspace = true\n"
                    "serde_json.workspace = true\n"
                    "sha2.workspace = true\n"
                    "thiserror.workspace = true"
                ),
                "ctx-history-provider-docproj": (
                    "ctx-history-capture-model = { path = \"../ctx-history-capture-model\" }\n"
                    "ctx-history-capture-runtime = { path = \"../ctx-history-capture-runtime\" }\n"
                    "ctx-history-core = { path = \"../ctx-history-core\" }\n"
                    "ctx-history-provider-runtime = { path = \"../ctx-history-provider-runtime\" }\n"
                    "ctx-history-source-discovery = { path = \"../ctx-history-source-discovery\" }\n"
                    "ctx-history-source-io = { path = \"../ctx-history-source-io\" }\n"
                    "ctx-history-source-sqlite = { path = \"../ctx-history-source-sqlite\" }\n"
                    "chrono.workspace = true\n"
                    "rusqlite.workspace = true\n"
                    "serde.workspace = true\n"
                    "serde_json.workspace = true\n"
                    "sha2.workspace = true\n"
                    "thiserror.workspace = true"
                ),
                "ctx-history-provider-runtime": (
                    "ctx-history-capture-model = { path = \"../ctx-history-capture-model\" }\n"
                    "ctx-history-capture-runtime = { path = \"../ctx-history-capture-runtime\" }\n"
                    "ctx-history-core = { path = \"../ctx-history-core\" }\n"
                    "ctx-history-jsonl = { path = \"../ctx-history-jsonl\" }"
                ),
                "ctx-history-provider-native-jsonl": (
                    "ctx-history-capture-model = { path = \"../ctx-history-capture-model\" }\n"
                    "ctx-history-capture-runtime = { path = \"../ctx-history-capture-runtime\" }\n"
                    "ctx-history-core = { path = \"../ctx-history-core\" }\n"
                    "ctx-history-jsonl = { path = \"../ctx-history-jsonl\" }\n"
                    "ctx-history-native-jsonl-parsers = { path = \"../ctx-history-native-jsonl-parsers\" }\n"
                    "ctx-history-source-io = { path = \"../ctx-history-source-io\" }\n"
                    "chrono.workspace = true\n"
                    "serde.workspace = true\n"
                    "serde_json.workspace = true\n"
                    "sha2.workspace = true\n"
                    "thiserror.workspace = true"
                ),
                "ctx-history-provider-codex": (
                    "ctx-history-capture-model = { path = \"../ctx-history-capture-model\" }\n"
                    "ctx-history-capture-runtime = { path = \"../ctx-history-capture-runtime\" }\n"
                    "ctx-history-core = { path = \"../ctx-history-core\" }\n"
                    "ctx-history-source-io = { path = \"../ctx-history-source-io\" }\n"
                    "chrono.workspace = true\n"
                    "serde.workspace = true\n"
                    "serde_json.workspace = true\n"
                    "sha2.workspace = true\n"
                    "tempfile.workspace = true\n"
                    "thiserror.workspace = true\n"
                    "uuid.workspace = true"
                ),
                "ctx-history-provider-trae": (
                    "ctx-history-capture-model = { path = \"../ctx-history-capture-model\" }\n"
                    "ctx-history-capture-runtime = { path = \"../ctx-history-capture-runtime\" }\n"
                    "ctx-history-core = { path = \"../ctx-history-core\" }\n"
                    "ctx-history-provider-runtime = { path = \"../ctx-history-provider-runtime\" }\n"
                    "ctx-history-source-io = { path = \"../ctx-history-source-io\" }\n"
                    "ctx-history-source-sqlite = { path = \"../ctx-history-source-sqlite\" }\n"
                    "chrono.workspace = true\n"
                    "rusqlite.workspace = true\n"
                    "serde.workspace = true\n"
                    "serde_json.workspace = true\n"
                    "sha2.workspace = true\n"
                    "thiserror.workspace = true\n"
                    "tempfile.workspace = true"
                ),
                "ctx-history-providers-task-docs": (
                    "base64.workspace = true\n"
                    "chrono.workspace = true\n"
                    "libc.workspace = true\n"
                    "regex.workspace = true\n"
                    "serde.workspace = true\n"
                    "serde_json.workspace = true\n"
                    "sha2.workspace = true\n"
                    "thiserror.workspace = true\n"
                    "uuid.workspace = true\n"
                    "ctx-history-capture-model = { path = \"../ctx-history-capture-model\" }\n"
                    "ctx-history-capture-runtime = { path = \"../ctx-history-capture-runtime\" }\n"
                    "ctx-history-core = { path = \"../ctx-history-core\" }\n"
                    "ctx-history-source-io = { path = \"../ctx-history-source-io\" }"
                ),
                "ctx-history-source-io": "",
                "ctx-history-source-discovery": "",
                "ctx-history-source-sqlite": "",
                "ctx-history-providers-jsonl-shared": (
                    "ctx-history-capture-model = { path = \"../ctx-history-capture-model\" }\n"
                    "ctx-history-capture-runtime = { path = \"../ctx-history-capture-runtime\" }\n"
                    "ctx-history-core = { path = \"../ctx-history-core\" }\n"
                    "ctx-history-jsonl = { path = \"../ctx-history-jsonl\" }\n"
                    "ctx-history-provider-runtime = { path = \"../ctx-history-provider-runtime\" }"
                ),
                "ctx-history-provider-mistral-mux": (
                    "ctx-history-capture-model = { path = \"../ctx-history-capture-model\" }\n"
                    "ctx-history-capture-runtime = { path = \"../ctx-history-capture-runtime\" }\n"
                    "ctx-history-core = { path = \"../ctx-history-core\" }\n"
                    "ctx-history-jsonl = { path = \"../ctx-history-jsonl\" }\n"
                    "ctx-history-provider-runtime = { path = \"../ctx-history-provider-runtime\" }\n"
                    "ctx-history-source-io = { path = \"../ctx-history-source-io\" }\n"
                    "chrono.workspace = true\n"
                    "serde.workspace = true\n"
                    "serde_json.workspace = true\n"
                    "sha2.workspace = true\n"
                    "tempfile.workspace = true\n"
                    "uuid.workspace = true"
                ),
                "ctx-history-providers-sqlite-selected": (
                    "ctx-history-capture-model = { path = \"../ctx-history-capture-model\" }\n"
                    "ctx-history-capture-runtime = { path = \"../ctx-history-capture-runtime\" }\n"
                    "ctx-history-core = { path = \"../ctx-history-core\" }\n"
                    "ctx-history-source-io = { path = \"../ctx-history-source-io\" }\n"
                    "ctx-history-source-sqlite = { path = \"../ctx-history-source-sqlite\" }"
                ),
                "ctx-history-providers-sqlite-inventory": (
                    "ctx-history-capture-model = { path = \"../ctx-history-capture-model\" }\n"
                    "ctx-history-capture-runtime = { path = \"../ctx-history-capture-runtime\" }\n"
                    "ctx-history-core = { path = \"../ctx-history-core\" }\n"
                    "ctx-history-provider-runtime = { path = \"../ctx-history-provider-runtime\" }\n"
                    "ctx-history-source-io = { path = \"../ctx-history-source-io\" }\n"
                    "ctx-history-source-sqlite = { path = \"../ctx-history-source-sqlite\" }"
                ),
                "ctx-history-providers-sqlite-logical": (
                    "ctx-history-capture-model = { path = \"../ctx-history-capture-model\" }\n"
                    "ctx-history-capture-runtime = { path = \"../ctx-history-capture-runtime\" }\n"
                    "ctx-history-core = { path = \"../ctx-history-core\" }\n"
                    "ctx-history-source-io = { path = \"../ctx-history-source-io\" }\n"
                    "ctx-history-source-sqlite = { path = \"../ctx-history-source-sqlite\" }\n"
                    "rmpv.workspace = true\n"
                    "serde_json.workspace = true"
                ),
                "ctx-history-capture-model": (
                    "ctx-history-core = { path = \"../ctx-history-core\" }\n"
                    "serde_json.workspace = true"
                ),
                "ctx-history-cli": (
                    "ctx-client-observability = { path = \"../ctx-client-observability\" }\n"
                    "ctx-daemon-cli = { path = \"../ctx-daemon-cli\" }\n"
                    "ctx-history-core = { path = \"../ctx-history-core\" }\n"
                    "ctx-history-index = { path = \"../ctx-history-index\" }\n"
                    "ctx-history-read-application = { path = \"../ctx-history-read-application\" }\n"
                    "ctx-history-refresh = { path = \"../ctx-history-refresh\" }\n"
                    "ctx-terminal = { path = \"../ctx-terminal\" }"
                ),
                "ctx-history-refresh": (
                    "ctx-history-core = { path = \"../ctx-history-core\" }\n"
                    "ctx-history-index = { path = \"../ctx-history-index\" }\n"
                    "ctx-history-refresh-execution = { path = \"../ctx-history-refresh-execution\" }"
                ),
                "ctx-history-capture-runtime": (
                    "ctx-history-capture-model = { path = \"../ctx-history-capture-model\" }\n"
                    "ctx-history-core = { path = \"../ctx-history-core\" }\n"
                    "uuid.workspace = true"
                ),
                "ctx-daemon-service": (
                    "ctx-client-observability = { path = \"../ctx-client-observability\" }\n"
                    "ctx-daemon-runtime = { path = \"../ctx-daemon-runtime\" }\n"
                    "ctx-history-core = { path = \"../ctx-history-core\" }\n"
                    "ctx-history-index = { path = \"../ctx-history-index\" }\n"
                    "ctx-semantic-index = { path = \"../ctx-semantic-index\" }\n"
                    "ctx-semantic-model = { path = \"../ctx-semantic-model\" }\n"
                    "ctx-upgrade-engine = { path = \"../ctx-upgrade-engine\" }"
                ),
                "ctx-history-read-application": (
                    "ctx-history-core = { path = \"../ctx-history-core\" }\n"
                    "ctx-history-index-format = { path = \"../ctx-history-index-format\" }\n"
                    "ctx-history-index-query = { path = \"../ctx-history-index-query\" }"
                ),
                "ctx-semantic-model": (
                    "ctx-history-core = { path = \"../ctx-history-core\" }"
                ),
                "ctx-managed-pair-engine": (
                    "ctx-history-platform = { path = \"../ctx-history-platform\" }"
                ),
                "ctx-upgrade-engine": (
                    "ctx-history-core = { path = \"../ctx-history-core\" }\n"
                    "ctx-managed-pair-engine = { path = \"../ctx-managed-pair-engine\" }"
                ),
            }.get(name)
            dependencies = (
                f"\n[dependencies]\n{dependencies}\n" if dependencies else ""
            )
            version = (
                'version = "1.0.0"'
                if name in {
                    "ctx-cli-presentation",
                    "ctx-daemon-application",
                    "ctx-daemon-cli",
                    "ctx-daemon-runtime",
                    "ctx-daemon-service",
                    "ctx-history-cli",
                    "ctx-terminal",
                }
                else "version.workspace = true"
            )
            manifest.write_text(
                f"""\
[package]
name = "{name}"
{version}
license.workspace = true
repository.workspace = true
{dependencies}""",
                encoding="utf-8",
            )
        self.index_manifest = (
            self.main_runfiles / "crates/ctx-history-index/Cargo.toml"
        )
        self.index_format_manifest = (
            self.main_runfiles / "crates/ctx-history-index-format/Cargo.toml"
        )
        self.index_query_manifest = (
            self.main_runfiles / "crates/ctx-history-index-query/Cargo.toml"
        )

        for name, version in EXTERNAL_PACKAGES:
            repository = f"{CRATE_REPOSITORY_PREFIX}crates__{name}-{version}"
            manifest = self.runfiles / repository / "Cargo.toml"
            manifest.parent.mkdir(parents=True)
            manifest.write_text(
                f"""\
[package]
name = "{name}"
version = "{version}"
license = "MIT OR Apache-2.0"
repository = "https://example.invalid/{name}"
""",
                encoding="utf-8",
            )
            (manifest.parent / "LICENSE").write_text(
                f"Synthetic license text for {name}.\n", encoding="utf-8"
            )

        inventory_labels = [
            "@@//crates/ctx-cli:ctx",
            "@@//crates/ctx-agent-application:ctx_agent_application",
            "@@//crates/ctx-agent-integrations:ctx_agent_integrations",
            "@@//crates/ctx-companion-bridge:ctx_companion_bridge",
            "@@//crates/ctx-cli-presentation:ctx_cli_presentation",
            "@@//crates/ctx-client-observability:ctx_client_observability",
            "@@//crates/ctx-daemon-application:ctx_daemon_application",
            "@@//crates/ctx-daemon-cli:ctx_daemon_cli",
            "@@//crates/ctx-daemon-runtime:ctx_daemon_runtime",
            "@@//crates/ctx-daemon-service:ctx_daemon_service",
            "@@//crates/ctx-history-capture:ctx_history_capture",
            "@@//crates/ctx-history-capture-composition:ctx_history_capture_composition",
            "@@//crates/ctx-history-capture-model:ctx_history_capture_model",
            "@@//crates/ctx-history-cli:ctx_history_cli",
            "@@//crates/ctx-history-capture-runtime:ctx_history_capture_runtime",
            "@@//crates/ctx-history-core:ctx_history_core",
            "@@//crates/ctx-history-index-format:ctx_history_index_format",
            "@@//crates/ctx-history-index:ctx_history_index",
            "@@//crates/ctx-history-index-query:ctx_history_index_query",
            "@@//crates/ctx-history-jsonl:ctx_history_jsonl",
            "@@//crates/ctx-history-platform:ctx_history_platform",
            "@@//crates/ctx-history-provider-claude-cursor:ctx_history_provider_claude_cursor",
            "@@//crates/ctx-history-provider-docproj:ctx_history_provider_docproj",
            "@@//crates/ctx-history-provider-gemini:ctx_history_provider_gemini",
            "@@//crates/ctx-history-provider-mistral-mux:ctx_history_provider_mistral_mux",
            "@@//crates/ctx-history-provider-native-jsonl:ctx_history_provider_native_jsonl",
            "@@//crates/ctx-history-provider-runtime:ctx_history_provider_runtime",
            "@@//crates/ctx-history-provider-codex:ctx_history_provider_codex",
            "@@//crates/ctx-history-provider-trae:ctx_history_provider_trae",
            "@@//crates/ctx-history-providers-sqlite-selected:ctx_history_providers_sqlite_selected",
            "@@//crates/ctx-history-providers-sqlite-inventory:ctx_history_providers_sqlite_inventory",
            "@@//crates/ctx-history-providers-sqlite-logical:ctx_history_providers_sqlite_logical",
            "@@//crates/ctx-history-providers-task-docs:ctx_history_providers_task_docs",
            "@@//crates/ctx-history-source-io:ctx_history_source_io",
            "@@//crates/ctx-history-source-discovery:ctx_history_source_discovery",
            "@@//crates/ctx-history-source-sqlite:ctx_history_source_sqlite",
            "@@//crates/ctx-history-refresh:ctx_history_refresh",
            "@@//crates/ctx-history-providers-jsonl-shared:ctx_history_providers_jsonl_shared",
            "@@//crates/ctx-history-refresh-execution:ctx_history_refresh_execution",
            "@@//crates/ctx-history-read-application:ctx_history_read_application",
            "@@//crates/ctx-managed-pair-engine:ctx_managed_pair_engine",
            "@@//crates/ctx-semantic-index:ctx_semantic_index",
            "@@//crates/ctx-semantic-model:ctx_semantic_model",
            "@@//crates/ctx-terminal:ctx_terminal",
            "@@//crates/ctx-upgrade-engine:ctx_upgrade_engine",
        ]
        inventory_labels.extend(
            f"@@{CRATE_REPOSITORY_PREFIX}crates__{name}-{version}//:{name}"
            for name, version in EXTERNAL_PACKAGES
        )
        self.target_inventory = self.root / "target-dependency-inventory.txt"
        self.target_inventory.write_text(
            "\n".join(sorted(inventory_labels)) + "\n", encoding="utf-8"
        )

        material_lines = [
            "main\tCargo.toml",
            "main\tLICENSE",
        ]
        material_lines.extend(
            f"main\t{directory}/Cargo.toml" for _, directory in WORKSPACE_PACKAGES
        )
        for name, version in EXTERNAL_PACKAGES:
            repository = f"{CRATE_REPOSITORY_PREFIX}crates__{name}-{version}"
            material_lines.extend(
                (
                    f"external\t{repository}/Cargo.toml",
                    f"external\t{repository}/LICENSE",
                )
            )
        tantivy_label = (
            f"@@{CRATE_REPOSITORY_PREFIX}crates__tantivy-0.26.1//:tantivy"
        )
        material_lines.extend(
            f"feature\t{tantivy_label}\t{feature}"
            for feature in TANTIVY_FEATURES
        )
        self.license_materials = self.root / "license-materials.txt"
        self.license_materials.write_text(
            "\n".join(sorted(material_lines)) + "\n", encoding="utf-8"
        )

        self.build_info = self.root / "ctx.build-info.json"
        self.write_build_info()
        self.sbom = self.root / "ctx.cdx.json"
        self.notices = self.root / "ctx.third-party-notices.txt"
        self.size_report = self.root / "ctx.size.json"
        self.candidate = self.root / "ctx.candidate.json"
        self.release_sums = self.root / "SHA256SUMS"
        self.runtime = self.root / "ctx-onnxruntime-windows-x64.zip"
        self.bound_candidate = self.root / "ctx.release-candidate.json"
        self.bound_digest = self.root / "ctx.release-candidate.json.sha256"
        self.handoff = self.root / "release-authority-handoff"
        self.expected_digest: str | None = None

    def tearDown(self) -> None:
        self.temporary.cleanup()


    def write_build_info(
        self,
        platform: str = "linux-x64",
        target: str = "x86_64-unknown-linux-gnu",
    ) -> None:
        self.build_info.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "artifact_sha256": hashlib.sha256(
                        self.artifact.read_bytes()
                    ).hexdigest(),
                    "cargo_lock_sha256": hashlib.sha256(
                        self.cargo_lock.read_bytes()
                    ).hexdigest(),
                    "platform": platform,
                    "target": target,
                    "source": {"commit": COMMIT, "clean": True},
                    "rust_version": "rustc 1.97.1 (test 2026-07-14)",
                    "builder": {
                        "base_image": {"actual": "sha256:" + "b" * 64},
                        "image_id": "sha256:" + "c" * 64,
                        "recipe_sha256": "d" * 64,
                    },
                },
                sort_keys=True,
                separators=(",", ":"),
            )
            + "\n",
            encoding="utf-8",
        )

    def configure_windows_release(self) -> None:
        self.target_id = "windows-x64"
        self.platform = "windows-x64"
        windows_artifact = self.root / "ctx.exe"
        self.artifact.rename(windows_artifact)
        self.artifact = windows_artifact
        self.build_info = self.root / "ctx.exe.build-info.json"
        self.sbom = self.root / "ctx.exe.cdx.json"
        self.notices = self.root / "ctx.exe.third-party-notices.txt"
        self.size_report = self.root / "ctx.exe.size.json"
        self.candidate = self.root / "ctx.exe.candidate.json"
        self.target_matrix.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "targets": [
                        {
                            "id": "windows-x64",
                            "public_rust_target": "x86_64-pc-windows-gnu",
                            "public_construction_authority": "linux-cross-cargo-zigbuild-v1",
                            "public_construction_label": "scripts/release/build-public-candidate-on-linux.sh",
                        }
                    ],
                },
                sort_keys=True,
                separators=(",", ":"),
            )
            + "\n",
            encoding="utf-8",
        )
        self.write_build_info("windows-x64", "x86_64-pc-windows-gnu")
        self.write_runtime()
        self.write_release_sums()

    def write_release_handoff(self) -> None:
        self.handoff.mkdir()
        copies = {
            "ctx.exe": self.artifact,
            "ctx.exe.build-info.json": self.build_info,
            "ctx.exe.cdx.json": self.sbom,
            "ctx.exe.size.json": self.size_report,
            "ctx.exe.third-party-notices.txt": self.notices,
            "SHA256SUMS": self.release_sums,
            "ctx-onnxruntime-windows-x64.zip": self.runtime,
            "ctx.exe.candidate.json": self.bound_candidate,
            "ctx.exe.candidate.json.sha256": self.bound_digest,
        }
        for name, source in copies.items():
            shutil.copyfile(source, self.handoff / name)
        for name in RELEASE_AUTHORITY_CANDIDATES:
            if name == "ctx.exe.candidate.json":
                continue
            payload = b"{}\n"
            (self.handoff / name).write_bytes(payload)
            (self.handoff / f"{name}.sha256").write_text(
                hashlib.sha256(payload).hexdigest() + "\n", encoding="ascii"
            )

    def write_runtime(
        self,
        dll: bytes = b"exact Windows runtime DLL\n",
        dll_name: str = "lib/onnxruntime.dll",
        omit: str | None = None,
        extra: str | None = None,
    ) -> None:
        with zipfile.ZipFile(
            self.runtime, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9
        ) as archive:
            directory = zipfile.ZipInfo(
                "lib/", date_time=(1980, 1, 1, 0, 0, 0)
            )
            directory.compress_type = zipfile.ZIP_DEFLATED
            directory.external_attr = 0o40755 << 16
            archive.writestr(directory, b"")
            for name in WINDOWS_RUNTIME_FILES:
                emitted_name = dll_name if name == "lib/onnxruntime.dll" else name
                if emitted_name == omit:
                    continue
                record = zipfile.ZipInfo(
                    emitted_name, date_time=(1980, 1, 1, 0, 0, 0)
                )
                record.compress_type = zipfile.ZIP_DEFLATED
                record.external_attr = 0o100644 << 16
                payload = dll if name == "lib/onnxruntime.dll" else f"{name}\n".encode()
                archive.writestr(record, payload)
            if extra is not None:
                record = zipfile.ZipInfo(
                    extra, date_time=(1980, 1, 1, 0, 0, 0)
                )
                record.compress_type = zipfile.ZIP_DEFLATED
                record.external_attr = 0o100644 << 16
                archive.writestr(record, b"unexpected\n")

    def write_release_sums(self) -> None:
        values = {
            name: hashlib.sha256(f"synthetic {name}\n".encode()).hexdigest()
            for name in LEGACY_RELEASE_ASSETS
        }
        values["ctx-windows-x64.exe"] = hashlib.sha256(
            self.artifact.read_bytes()
        ).hexdigest()
        values["ctx-onnxruntime-windows-x64.zip"] = hashlib.sha256(
            self.runtime.read_bytes()
        ).hexdigest()
        self.release_sums.write_text(
            "".join(f"{values[name]}  {name}\n" for name in LEGACY_RELEASE_ASSETS),
            encoding="ascii",
        )

    def command(self, mode: str) -> list[str]:
        if mode == "verify-release":
            expected = self.expected_digest or self.bound_digest.read_text(
                encoding="ascii"
            ).strip()
            return [
                sys.executable,
                "-I",
                str(SCRIPT),
                mode,
                "--handoff-dir",
                str(self.handoff),
                "--expected-manifest-sha256",
                expected,
            ]
        command = [
            sys.executable,
            "-I",
            str(SCRIPT),
            mode,
            "--artifact",
            str(self.artifact),
            "--build-info",
            str(self.build_info),
        ]
        if mode in ("verify-bundle", "bind-release"):
            candidate = self.candidate
            command.extend(
                [
                    "--sbom",
                    str(self.sbom),
                    "--notices",
                    str(self.notices),
                    "--size-report",
                    str(self.size_report),
                    "--candidate-manifest",
                    str(candidate),
                ]
            )
            if mode == "bind-release":
                command.extend(
                    [
                        "--release-sums",
                        str(self.release_sums),
                        "--runtime-archive",
                        str(self.runtime),
                    ]
                )
            if mode == "bind-release":
                command.extend(
                    [
                        "--output-manifest",
                        str(self.bound_candidate),
                        "--manifest-sha256-output",
                        str(self.bound_digest),
                    ]
                )
            return command
        command.extend(
            [
                "--product",
                "core",
                "--version",
                "0.26.0",
                "--target-id",
                self.target_id,
                "--platform",
                self.platform,
                "--cargo-lock",
                str(self.cargo_lock),
                "--module-lock",
                str(self.module_lock),
                "--module-file",
                str(self.module_file),
                "--target-inventory",
                str(self.target_inventory),
                "--license-materials",
                str(self.license_materials),
                "--target-matrix",
                str(self.target_matrix),
                "--candidate-schema",
                str(self.candidate_schema),
                "--workspace-manifest",
                str(self.workspace_manifest),
                "--index-manifest",
                str(self.index_manifest),
                "--index-format-manifest",
                str(self.index_format_manifest),
                "--index-query-manifest",
                str(self.index_query_manifest),
                "--runfiles-root",
                str(self.runfiles),
                "--candidate-manifest",
                str(self.candidate),
            ]
        )
        if mode == "generate":
            command.extend(
                (
                    "--output",
                    str(self.sbom),
                    "--notices-output",
                    str(self.notices),
                    "--size-report-output",
                    str(self.size_report),
                )
            )
        else:
            command.extend(
                (
                    "--sbom",
                    str(self.sbom),
                    "--notices",
                    str(self.notices),
                    "--size-report",
                    str(self.size_report),
                )
            )
        return command

    def run_command(
        self, mode: str, check: bool = True
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            self.command(mode),
            check=check,
            capture_output=True,
            text=True,
        )

    def generate(self) -> str:
        return self.run_command("generate").stdout.strip()

    def test_bundle_is_deterministic_license_complete_and_strictly_verifiable(
        self,
    ) -> None:
        first_digest = self.generate()
        first = {
            path: path.read_bytes()
            for path in (self.sbom, self.notices, self.size_report, self.candidate)
        }
        second_digest = self.generate()
        self.assertEqual(first_digest, second_digest)
        self.assertEqual(
            first,
            {
                path: path.read_bytes()
                for path in (self.sbom, self.notices, self.size_report, self.candidate)
            },
        )
        self.assertEqual(self.run_command("verify").stdout.strip(), first_digest)
        self.assertEqual(
            self.run_command("verify-bundle").stdout.strip(), first_digest
        )

        document = json.loads(self.sbom.read_bytes())
        cargo_components = [
            component
            for component in document["components"]
            if any(
                item["name"] == "ctx:dependency:ecosystem"
                for item in component.get("properties", [])
            )
        ]
        self.assertEqual(
            len(cargo_components), len(WORKSPACE_PACKAGES) + len(EXTERNAL_PACKAGES)
        )
        self.assertEqual(
            {component["name"] for component in cargo_components},
            {name for name, _ in WORKSPACE_PACKAGES}
            | {name for name, _ in EXTERNAL_PACKAGES},
        )
        self.assertTrue(
            all(component.get("licenses") for component in cargo_components)
        )
        sqlite_inventory = next(
            component
            for component in cargo_components
            if component["name"] == "ctx-history-providers-sqlite-inventory"
        )
        self.assertEqual(
            sqlite_inventory["version"], SYNTHETIC_WORKSPACE_VERSION
        )
        cargo_components_by_ref = {
            component["bom-ref"]: component for component in cargo_components
        }
        cargo_dependencies_by_ref = {
            dependency["ref"]: dependency["dependsOn"]
            for dependency in document["dependencies"]
            if dependency["ref"] in cargo_components_by_ref
        }
        document_projection = next(
            component
            for component in cargo_components
            if component["name"] == "ctx-history-provider-docproj"
        )
        self.assertEqual(
            {
                cargo_components_by_ref[dependency]["name"]
                for dependency in cargo_dependencies_by_ref[
                    document_projection["bom-ref"]
                ]
            },
            DOCUMENT_PROJECTION_DIRECT_DEPENDENCIES,
        )
        tantivy = next(
            component
            for component in cargo_components
            if component["name"] == "tantivy"
        )
        tantivy_properties = {
            item["name"]: json.loads(item["value"])
            if item["value"].startswith(("[", "{"))
            else item["value"]
            for item in tantivy["properties"]
        }
        self.assertEqual(
            tantivy_properties["ctx:rust:resolved-crate-features"],
            list(TANTIVY_FEATURES),
        )

        candidate = json.loads(self.candidate.read_bytes())
        expected_evidence = {
            "binary_size_report",
            "build_info",
            "candidate_schema",
            "cargo_lock",
            "ctx_history_index_manifest",
            "ctx_history_index_format_manifest",
            "ctx_history_index_query_manifest",
            "cyclonedx_sbom",
            "license_materials_inventory",
            "module_file",
            "module_lock",
            "target_dependency_inventory",
            "target_matrix",
            "third_party_notices",
            "workspace_manifest",
        }
        evidence_schema = json.loads(
            self.candidate_schema.read_text(encoding="utf-8")
        )["properties"]["evidence"]
        self.assertEqual(set(candidate["evidence"]), expected_evidence)
        self.assertEqual(set(evidence_schema["required"]), expected_evidence)
        self.assertEqual(
            set(evidence_schema["propertyNames"]["enum"]), expected_evidence
        )
        self.assertEqual(
            candidate["construction"],
            {
                "authority": "linux-cross-cargo-zigbuild-v1",
                "label": "scripts/release/build-public-candidate-on-linux.sh",
            },
        )
        self.assertEqual(
            candidate["tantivy"]["resolved_crate_features"],
            list(TANTIVY_FEATURES),
        )
        closure_names = {
            package["name"]
            for package in candidate["tantivy"]["dependency_closure"]
        }
        self.assertTrue(
            {"tantivy", "fs4", "lz4_flex", "memmap2", "tempfile", "zstd"}
            <= closure_names
        )
        self.assertIn("tantivy 0.26.1", self.notices.read_text(encoding="utf-8"))
        self.assertIn("thiserror 1.0.0", self.notices.read_text(encoding="utf-8"))
        self.assertIn(
            "Synthetic license text for tantivy.",
            self.notices.read_text(encoding="utf-8"),
        )
        size = json.loads(self.size_report.read_bytes())
        self.assertEqual(size["artifact"]["size_bytes"], self.artifact.stat().st_size)

    def test_unselected_lock_package_is_not_reported(self) -> None:
        self.cargo_lock.write_text(
            self.cargo_lock.read_text(encoding="utf-8")
            + "\n"
            + self.package("target-only", "9.9.9", external=True)
            + "\n",
            encoding="utf-8",
        )
        self.write_build_info()
        self.generate()
        names = {
            component["name"]
            for component in json.loads(self.sbom.read_bytes())["components"]
        }
        self.assertNotIn("target-only", names)

    def test_task_document_pack_omission_is_rejected(self) -> None:
        pack_label = "@@//crates/ctx-history-providers-task-docs:ctx_history_providers_task_docs"
        labels = self.target_inventory.read_text(encoding="utf-8").splitlines()
        self.assertIn(pack_label, labels)
        self.target_inventory.write_text(
            "\n".join(label for label in labels if label != pack_label) + "\n",
            encoding="utf-8",
        )
        rejected = self.run_command("generate", check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn(
            "target dependency inventory omits release workspace packages: "
            "ctx-history-providers-task-docs",
            rejected.stderr,
        )

    def test_document_projection_release_package_omission_is_rejected(self) -> None:
        omitted = "@@//crates/ctx-history-provider-docproj:ctx_history_provider_docproj"
        labels = self.target_inventory.read_text(encoding="utf-8").splitlines()
        self.assertIn(omitted, labels)
        self.target_inventory.write_text(
            "\n".join(label for label in labels if label != omitted) + "\n",
            encoding="utf-8",
        )
        rejected = self.run_command("generate", check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn(
            "target dependency inventory omits release workspace packages: "
            "ctx-history-provider-docproj",
            rejected.stderr,
        )

    def test_companion_bridge_release_package_omission_is_rejected(self) -> None:
        omitted = "@@//crates/ctx-companion-bridge:ctx_companion_bridge"
        labels = self.target_inventory.read_text(encoding="utf-8").splitlines()
        self.assertIn(omitted, labels)
        self.target_inventory.write_text(
            "\n".join(label for label in labels if label != omitted) + "\n",
            encoding="utf-8",
        )
        rejected = self.run_command("generate", check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn(
            "target dependency inventory omits release workspace packages: "
            "ctx-companion-bridge",
            rejected.stderr,
        )

    def test_missing_license_expression_is_rejected(self) -> None:
        manifest = (
            self.runfiles
            / f"{CRATE_REPOSITORY_PREFIX}crates__tantivy-0.26.1/Cargo.toml"
        )
        manifest.write_text(
            manifest.read_text(encoding="utf-8").replace(
                'license = "MIT OR Apache-2.0"\n', ""
            ),
            encoding="utf-8",
        )
        rejected = self.run_command("generate", check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("has no license expression", rejected.stderr)

    def test_tantivy_feature_drift_is_rejected(self) -> None:
        value = self.license_materials.read_text(encoding="utf-8")
        value = value.replace(
            "\tcolumnar-zstd-compression\n",
            "\tstopwords\n",
        )
        self.license_materials.write_text(
            "\n".join(sorted(value.splitlines())) + "\n",
            encoding="utf-8",
        )
        rejected = self.run_command("generate", check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("configured Bazel Tantivy features", rejected.stderr)

    def test_substituted_artifact_or_evidence_is_rejected(self) -> None:
        self.generate()
        original_artifact = self.artifact.read_bytes()
        self.artifact.write_bytes(b"substituted artifact\n")
        rejected = self.run_command("verify-bundle", check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("candidate manifest does not bind", rejected.stderr)

        self.artifact.write_bytes(original_artifact)
        candidate = json.loads(self.candidate.read_bytes())
        candidate["tantivy"]["dependency_closure"].pop()
        self.candidate.write_text(
            json.dumps(candidate, sort_keys=True, separators=(",", ":")) + "\n",
            encoding="utf-8",
        )
        rejected = self.run_command("verify-bundle", check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("Tantivy contract is malformed", rejected.stderr)

        self.generate()
        self.notices.write_bytes(self.notices.read_bytes() + b"mutation\n")
        rejected = self.run_command("verify-bundle", check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("does not bind third_party_notices", rejected.stderr)

    def test_windows_release_manifest_binds_sums_runtime_dll_and_authority(
        self,
    ) -> None:
        self.configure_windows_release()
        self.generate()
        authority = self.run_command("bind-release").stdout.strip()
        self.write_release_handoff()
        self.assertEqual(authority, self.bound_digest.read_text().strip())
        self.assertEqual(self.run_command("verify-release").stdout.strip(), authority)

        candidate = json.loads(self.bound_candidate.read_bytes())
        self.assertEqual(
            candidate["release_sums"],
            {
                "file": "SHA256SUMS",
                "sha256": hashlib.sha256(self.release_sums.read_bytes()).hexdigest(),
                "size_bytes": self.release_sums.stat().st_size,
            },
        )
        with zipfile.ZipFile(self.runtime) as archive:
            dll = archive.read("lib/onnxruntime.dll")
        self.assertEqual(
            candidate["runtime"],
            {
                "file": "ctx-onnxruntime-windows-x64.zip",
                "sha256": hashlib.sha256(self.runtime.read_bytes()).hexdigest(),
                "size_bytes": self.runtime.stat().st_size,
                "dll": {
                    "file": "lib/onnxruntime.dll",
                    "sha256": hashlib.sha256(dll).hexdigest(),
                    "size_bytes": len(dll),
                },
            },
        )

        handoff_sums = self.handoff / "SHA256SUMS"
        original_sums = handoff_sums.read_bytes()
        lines = original_sums.decode("ascii").splitlines()
        replacement = "0" if lines[0][0] != "0" else "1"
        lines[0] = replacement + lines[0][1:]
        handoff_sums.write_text("\n".join(lines) + "\n", encoding="ascii")
        rejected = self.run_command("verify-release", check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("does not bind exact release SHA256SUMS", rejected.stderr)
        handoff_sums.write_bytes(original_sums)

        self.write_runtime(b"X" * len(dll))
        shutil.copyfile(
            self.runtime, self.handoff / "ctx-onnxruntime-windows-x64.zip"
        )
        rejected = self.run_command("verify-release", check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("does not bind exact Windows runtime and DLL", rejected.stderr)

        # A complete caller-coordinated replacement remains unauthorized: even
        # canonical regenerated manifest/sums/archive/DLL records cannot change
        # the digest already committed by signed or attested release metadata.
        original_authority = authority
        self.write_release_sums()
        self.bound_candidate.unlink()
        self.bound_digest.unlink()
        self.run_command("bind-release")
        shutil.rmtree(self.handoff)
        self.write_release_handoff()
        self.expected_digest = original_authority
        rejected = self.run_command("verify-release", check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("independently supplied expected digest", rejected.stderr)

    def test_verify_release_handoff_requires_exact_construction_names(self) -> None:
        self.configure_windows_release()
        self.generate()
        authority = self.run_command("bind-release").stdout.strip()
        self.write_release_handoff()

        self.assertTrue((self.handoff / "ctx.exe").is_file())
        self.assertFalse((self.handoff / "ctx-windows-x64.exe").exists())
        self.assertIn(
            "  ctx-windows-x64.exe\n",
            (self.handoff / "SHA256SUMS").read_text(encoding="ascii"),
        )
        self.assertEqual(self.run_command("verify-release").stdout.strip(), authority)

        (self.handoff / "ctx.exe").rename(
            self.handoff / "ctx-windows-x64.exe"
        )
        rejected = self.run_command("verify-release", check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("exact production inventory", rejected.stderr)

    def test_verify_release_accepts_only_the_handoff_interface(self) -> None:
        self.configure_windows_release()
        self.generate()
        self.run_command("bind-release")
        self.write_release_handoff()
        command = self.command("verify-release")
        command.extend(("--artifact", str(self.artifact)))
        rejected = subprocess.run(command, capture_output=True, text=True, check=False)
        self.assertEqual(rejected.returncode, 2)
        self.assertIn("only through --handoff-dir", rejected.stderr)

    def test_windows_release_binding_requires_literal_outer_and_dll_names(self) -> None:
        self.configure_windows_release()
        self.generate()

        renamed_sums = self.root / "OTHER_SUMS"
        renamed_sums.write_bytes(self.release_sums.read_bytes())
        command = self.command("bind-release")
        command[command.index(str(self.release_sums))] = str(renamed_sums)
        rejected = subprocess.run(command, capture_output=True, text=True, check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("must be named SHA256SUMS", rejected.stderr)

        renamed_runtime = self.root / "other-runtime.zip"
        renamed_runtime.write_bytes(self.runtime.read_bytes())
        command = self.command("bind-release")
        command[command.index(str(self.runtime))] = str(renamed_runtime)
        rejected = subprocess.run(command, capture_output=True, text=True, check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("must be named ctx-onnxruntime-windows-x64.zip", rejected.stderr)

        self.write_runtime(dll_name="lib/other.dll")
        self.write_release_sums()
        rejected = self.run_command("bind-release", check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("unexpected entry lib/other.dll", rejected.stderr)

    def test_windows_release_binding_requires_exact_runtime_and_sums_inventories(
        self,
    ) -> None:
        self.configure_windows_release()
        self.generate()

        self.write_runtime(omit="lib/vcruntime140_1.dll")
        self.write_release_sums()
        rejected = self.run_command("bind-release", check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("do not exactly match the legacy sidecar layout", rejected.stderr)

        self.write_runtime(extra="lib/extra.dll")
        self.write_release_sums()
        rejected = self.run_command("bind-release", check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("unexpected entry lib/extra.dll", rejected.stderr)

        self.write_runtime()
        self.write_release_sums()
        lines = self.release_sums.read_text(encoding="ascii").splitlines()
        self.release_sums.write_text(
            "\n".join(reversed(lines)) + "\n", encoding="ascii"
        )
        rejected = self.run_command("bind-release", check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("exact canonical 20- or 29-entry", rejected.stderr)


if __name__ == "__main__":
    unittest.main()
