#!/usr/bin/env python3
"""Mutation coverage for the Trae provider ownership boundary."""

import tempfile
import unittest
from pathlib import Path

from check_history_provider_trae_boundary import BoundaryError, validate


class TraeBoundaryMutationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        root = Path(self.temporary.name)
        self.manifest = root / "Cargo.toml"
        self.build = root / "BUILD.bazel"
        self.source = root / "pack" / "src"
        self.facade = root / "capture" / "providers" / "trae.rs"
        self.registration = root / "capture" / "registration.rs"
        self.discovery = root / "capture" / "provider_sources.rs"
        self.source.mkdir(parents=True)
        self.facade.parent.mkdir(parents=True)
        self.registration.parent.mkdir(parents=True, exist_ok=True)

        regular = {
            "chrono",
            "ctx-history-capture-model",
            "ctx-history-capture-runtime",
            "ctx-history-core",
            "ctx-history-provider-runtime",
            "ctx-history-source-io",
            "ctx-history-source-sqlite",
            "rusqlite",
            "serde",
            "serde_json",
            "sha2",
            "thiserror",
        }
        self.manifest.write_text(
            '[package]\nname = "ctx-history-provider-trae"\nversion.workspace = true\n'
            "[dependencies]\n"
            + "".join(
                f'{dependency} = {{ path = "../{dependency}" }}\n'
                if dependency.startswith("ctx-history-")
                else f"{dependency}.workspace = true\n"
                for dependency in sorted(regular)
            )
            + "[dev-dependencies]\ntempfile.workspace = true\n",
            encoding="utf-8",
        )
        self.build.write_text(
            "\n".join(
                [
                    'crate_name = "ctx_history_provider_trae"',
                    'name = "test_support_lib"',
                    *(
                        f'"//crates/{package}:lib"'
                        for package in (
                            "ctx-history-capture-model",
                            "ctx-history-capture-runtime",
                            "ctx-history-core",
                            "ctx-history-provider-runtime",
                            "ctx-history-source-io",
                            "ctx-history-source-sqlite",
                        )
                    ),
                ]
            ),
            encoding="utf-8",
        )
        (self.source / "lib.rs").write_text(
            "ProviderRuntimeBinding ReplacementDocumentTree "
            "ProviderChangedDocumentSink TRAE_CHAT_ROWS_QUERY TRAE_CHAT_KEYS",
            encoding="utf-8",
        )
        self.facade.write_text(
            "pub(crate) use ctx_history_provider_trae; "
            "ctx_history_provider_trae::TraeReplacementTree<CaptureProviderRuntime>",
            encoding="utf-8",
        )
        self.registration.write_text(
            "fn register_trae_route() { "
            "TraeReplacementTree::new(data_root, source.path.clone()); "
            "register_replacement_document_tree_route_with_authority(); "
            "SourceBackedSelectorAuthority::DiscoveredWinner; }",
            encoding="utf-8",
        )
        self.discovery.write_text(
            "TraeProbeFragment::new classify_trae_payload_for_discovery "
            "trae_payload_admission TRAE_CHAT_ROWS_QUERY",
            encoding="utf-8",
        )

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def validate(self) -> None:
        validate(
            self.manifest,
            self.build,
            self.source,
            self.facade,
            self.registration,
            self.discovery,
        )

    def test_passes(self) -> None:
        self.validate()

    def test_version_inheritance_is_required(self) -> None:
        self.manifest.write_text(
            self.manifest.read_text(encoding="utf-8").replace(
                "version.workspace = true", 'version = "0.26.0"'
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "version.workspace"):
            self.validate()

    def test_capture_dependency_is_rejected(self) -> None:
        self.build.write_text(
            self.build.read_text(encoding="utf-8")
            + '\n"//crates/ctx-history-capture:lib"',
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "capture/index"):
            self.validate()

    def test_index_source_authority_is_rejected(self) -> None:
        (self.source / "extra.rs").write_text(
            "use ctx_history_index::IndexError;", encoding="utf-8"
        )
        with self.assertRaisesRegex(BoundaryError, "forbidden authority"):
            self.validate()

    def test_capture_binding_is_required(self) -> None:
        self.facade.write_text(
            "pub(crate) use ctx_history_provider_trae;", encoding="utf-8"
        )
        with self.assertRaisesRegex(BoundaryError, "capture Trae facade"):
            self.validate()

    def test_discovered_winner_route_authority_is_required(self) -> None:
        self.registration.write_text(
            self.registration.read_text(encoding="utf-8").replace(
                "SourceBackedSelectorAuthority::DiscoveredWinner", ""
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "route registration"):
            self.validate()

    def test_discovery_probe_is_required(self) -> None:
        self.discovery.write_text("trae_payload_admission", encoding="utf-8")
        with self.assertRaisesRegex(BoundaryError, "discovery probe"):
            self.validate()

    def test_duplicate_capture_implementation_is_rejected(self) -> None:
        duplicate = self.facade.with_suffix("") / "event.rs"
        duplicate.parent.mkdir()
        duplicate.write_text("struct Duplicate;", encoding="utf-8")
        with self.assertRaisesRegex(BoundaryError, "duplicate"):
            self.validate()


if __name__ == "__main__":
    unittest.main()
