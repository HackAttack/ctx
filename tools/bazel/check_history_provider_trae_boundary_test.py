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
        self.capture_facade = root / "capture" / "providers" / "trae.rs"
        self.composition_manifest = root / "composition" / "Cargo.toml"
        self.composition_build = root / "composition" / "BUILD.bazel"
        self.composition_facade = root / "composition" / "src" / "lib.rs"
        self.registration = root / "composition" / "registration.rs"
        self.discovery = root / "capture" / "provider_sources.rs"
        self.source.mkdir(parents=True)
        self.capture_facade.parent.mkdir(parents=True)
        self.composition_facade.parent.mkdir(parents=True)
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
        self.capture_facade.write_text(
            "pub(crate) use ctx_history_provider_trae::{ "
            "trae_payload_admission, TraePayloadAdmission, TRAE_CHAT_KEYS, "
            "TRAE_CHAT_ROWS_QUERY, TRAE_SQLITE_VALUE_OVERHEAD_BYTES };",
            encoding="utf-8",
        )
        self.composition_manifest.write_text(
            '[package]\nname = "ctx-history-capture-composition"\n'
            "[dependencies]\n"
            'ctx-history-provider-trae = { path = "../ctx-history-provider-trae" }\n',
            encoding="utf-8",
        )
        self.composition_build.write_text(
            'COMPOSITION_DEPS = [\n    "//crates/ctx-history-provider-trae:lib",\n]\n',
            encoding="utf-8",
        )
        self.composition_facade.write_text(
            "pub(crate) type TraeReplacementTree = "
            "ctx_history_provider_trae::TraeReplacementTree<"
            "crate::source_backed::family::CaptureProviderRuntime>;",
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
            self.capture_facade,
            self.composition_manifest,
            self.composition_build,
            self.composition_facade,
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

    def test_capture_probe_imports_are_required(self) -> None:
        self.capture_facade.write_text(
            self.capture_facade.read_text(encoding="utf-8").replace(
                "TRAE_CHAT_ROWS_QUERY", ""
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "capture Trae facade"):
            self.validate()

    def test_capture_replacement_tree_alias_is_rejected(self) -> None:
        self.capture_facade.write_text(
            self.capture_facade.read_text(encoding="utf-8")
            + " ctx_history_provider_trae::TraeReplacementTree<"
            "CaptureProviderRuntime>;",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "forbidden authority"):
            self.validate()

    def test_composition_cargo_dependency_must_be_active(self) -> None:
        dependency = (
            'ctx-history-provider-trae = { path = "../ctx-history-provider-trae" }\n'
        )
        original = self.composition_manifest.read_text(encoding="utf-8")
        for replacement in ("", "# " + dependency):
            with self.subTest(replacement=replacement):
                self.composition_manifest.write_text(
                    original.replace(dependency, replacement), encoding="utf-8"
                )
                with self.assertRaisesRegex(BoundaryError, "Cargo dependency"):
                    self.validate()

    def test_composition_production_dependency_is_exactly_once(self) -> None:
        label = '"//crates/ctx-history-provider-trae:lib"'
        for dependency_list in (
            "[]",
            f"[\n    # {label},\n]",
            f"[\n    {label},\n    {label},\n]",
            '["//crates/ctx-history-provider-trae:test_support_lib"]',
        ):
            with self.subTest(dependency_list=dependency_list):
                self.composition_build.write_text(
                    f"COMPOSITION_DEPS = {dependency_list}\n", encoding="utf-8"
                )
                with self.assertRaisesRegex(BoundaryError, "production dependencies"):
                    self.validate()

    def test_composition_production_dependency_must_be_literal(self) -> None:
        for value in ("TRAE_DEPS", "[TRAE_LABEL]", "["):
            with self.subTest(value=value):
                self.composition_build.write_text(
                    f"COMPOSITION_DEPS = {value}\n", encoding="utf-8"
                )
                with self.assertRaisesRegex(BoundaryError, "dependency inventory"):
                    self.validate()

    def test_commented_capture_alias_is_rejected(self) -> None:
        self.capture_facade.write_text(
            "// " + self.capture_facade.read_text(encoding="utf-8"), encoding="utf-8"
        )
        with self.assertRaisesRegex(BoundaryError, "capture Trae facade"):
            self.validate()

    def test_commented_runtime_binding_is_rejected(self) -> None:
        self.composition_facade.write_text(
            self.composition_facade.read_text(encoding="utf-8").replace(
                "crate::source_backed::family::CaptureProviderRuntime",
                "// crate::source_backed::family::CaptureProviderRuntime",
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "composition Trae facade"):
            self.validate()

    def test_commented_registration_is_rejected(self) -> None:
        self.registration.write_text(
            "/* " + self.registration.read_text(encoding="utf-8") + " */",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "route registration"):
            self.validate()

    def test_missing_expected_input_is_rejected(self) -> None:
        self.registration.unlink()
        with self.assertRaises(OSError):
            self.validate()

    def test_composition_replacement_tree_alias_is_required(self) -> None:
        self.composition_facade.write_text("", encoding="utf-8")
        with self.assertRaisesRegex(BoundaryError, "composition Trae facade"):
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
        duplicate = self.capture_facade.with_suffix("") / "event.rs"
        duplicate.parent.mkdir()
        duplicate.write_text("struct Duplicate;", encoding="utf-8")
        with self.assertRaisesRegex(BoundaryError, "duplicate"):
            self.validate()


if __name__ == "__main__":
    unittest.main()
