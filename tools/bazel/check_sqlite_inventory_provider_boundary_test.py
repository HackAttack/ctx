#!/usr/bin/env python3
"""Adversarial mutations for the SQLite inventory provider boundary."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from check_sqlite_inventory_provider_boundary import (
    BoundaryError,
    EXPECTED_INTERNAL,
    validate_capture_ownership,
    validate_manifest,
    validate_pack_sources,
)


class SqliteInventoryProviderBoundaryMutations(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        providers = self.root / "provider/providers"
        providers.mkdir(parents=True)
        (providers / "mod.rs").write_text(
            "pub mod astrbot;\npub mod crush;\npub mod hermes;\npub mod lingma;\npub mod shelley;\n",
            encoding="utf-8",
        )
        (self.root / "registration.rs").write_text(
            "pub fn astrbot_registration() {}\npub fn crush_registration() {}\n"
            "pub fn discovered_lingma_registration() {}\npub fn hermes_automatic_registration() {}\n"
            "pub fn hermes_explicit_registration() {}\npub fn lingma_registration() {}\n"
            "pub fn shelley_registration() {}\n",
            encoding="utf-8",
        )
        (self.root / "registration").mkdir()
        (self.root / "registration/crush.rs").write_text("", encoding="utf-8")
        self.capture_root = self.root / "capture"
        capture_providers = self.capture_root / "provider/providers"
        capture_providers.mkdir(parents=True)
        (capture_providers / "mod.rs").write_text("", encoding="utf-8")
        facade = (
            self.capture_root
            / "provider/source_backed/registration/families/sqlite_inventory.rs"
        )
        facade.parent.mkdir(parents=True)
        facade.write_text(
            "use ctx_history_providers_sqlite_inventory::registration::{\n"
            "    astrbot_registration, crush_registration, hermes_explicit_registration,\n"
            "    lingma_registration, shelley_registration,\n"
            "};\n"
            "pub fn register_astrbot_source_backed_route() {\n"
            "    install_sqlite_inventory_registration(astrbot_registration::<L, S>());\n"
            "}\n"
            "pub fn register_crush_source_backed_route() {\n"
            "    install_sqlite_inventory_registration(crush_registration::<I, L, S>());\n"
            "}\n"
            "pub fn register_hermes_explicit_source_backed_route() {\n"
            "    install_sqlite_inventory_registration(hermes_explicit_registration::<L, S>());\n"
            "}\n"
            "pub fn register_lingma_source_backed_route() {\n"
            "    install_sqlite_inventory_registration(lingma_registration::<L, S>());\n"
            "}\n"
            "pub fn register_shelley_source_backed_route() {\n"
            "    install_sqlite_inventory_registration(shelley_registration::<L, S>());\n"
            "}\n",
            encoding="utf-8",
        )
        self.manifest = self.root / "Cargo.toml"
        dependencies = "".join(
            f'{dependency} = "1"\n' for dependency in sorted(EXPECTED_INTERNAL)
        )
        self.manifest.write_text(
            "[features]\n"
            "test-support = []\n"
            "\n"
            "[dependencies]\n"
            f"{dependencies}"
            "\n"
            "[dev-dependencies]\n",
            encoding="utf-8",
        )

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def validate(self) -> None:
        validate_manifest(self.manifest)
        validate_pack_sources(self.root)
        validate_capture_ownership(self.capture_root)

    def append_manifest(self, contents: str) -> None:
        with self.manifest.open("a", encoding="utf-8") as manifest:
            manifest.write(contents)

    def test_extra_pack_registration_owner_is_rejected(self) -> None:
        registration = self.root / "registration.rs"
        registration.write_text(
            registration.read_text(encoding="utf-8")
            + "pub(crate) fn duplicate_registration() {}\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "registration authority drifted"):
            self.validate()

    def test_digit_bearing_extra_pack_registration_owner_is_rejected(self) -> None:
        registration = self.root / "registration.rs"
        registration.write_text(
            registration.read_text(encoding="utf-8")
            + "pub(crate) fn duplicate2_registration() {}\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(
            BoundaryError, r"extra=\['duplicate2_registration'\]"
        ):
            self.validate()

    def test_digit_bearing_extra_provider_module_is_rejected(self) -> None:
        providers = self.root / "provider/providers/mod.rs"
        providers.write_text(
            providers.read_text(encoding="utf-8") + "pub(crate) mod extra2;\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, r"actual=.*extra2"):
            self.validate()

    def test_duplicate_expected_pack_registration_owner_is_rejected(self) -> None:
        (self.root / "duplicate_owner.rs").write_text(
            "pub fn astrbot_registration() {}\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(
            BoundaryError, r"duplicates=\['astrbot_registration'\]"
        ):
            self.validate()

    def test_capture_facade_growth_is_rejected(self) -> None:
        facade = (
            self.capture_root
            / "provider/source_backed/registration/families/sqlite_inventory.rs"
        )
        facade.write_text(
            facade.read_text(encoding="utf-8")
            + "pub fn register_duplicate_source_backed_route() {}\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "façade item surface drifted"):
            self.validate()

    def test_digit_bearing_extra_capture_facade_binding_is_rejected(self) -> None:
        facade = (
            self.capture_root
            / "provider/source_backed/registration/families/sqlite_inventory.rs"
        )
        facade.write_text(
            facade.read_text(encoding="utf-8").replace(
                "    lingma_registration, shelley_registration,\n",
                "    duplicate2_registration, lingma_registration, "
                "shelley_registration,\n",
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(
            BoundaryError, r"extra=\['duplicate2_registration'\]"
        ):
            self.validate()

    def test_digit_bearing_extra_capture_facade_call_is_rejected(self) -> None:
        facade = (
            self.capture_root
            / "provider/source_backed/registration/families/sqlite_inventory.rs"
        )
        facade.write_text(
            facade.read_text(encoding="utf-8")
            .replace(
                "    lingma_registration, shelley_registration,\n",
                "    duplicate2_registration, lingma_registration, "
                "shelley_registration,\n",
            )
            .replace(
                "    install_sqlite_inventory_registration(astrbot_registration::<L, S>());\n",
                "    install_sqlite_inventory_registration(astrbot_registration::<L, S>());\n"
                "    duplicate2_registration::<L, S>();\n",
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(
            BoundaryError,
            r"constructor calls drifted: unexpected=\['duplicate2_registration'\]",
        ):
            self.validate()

    def test_inline_cfg_test_open_options_fixture_is_excluded(self) -> None:
        (self.root / "provider.rs").write_text(
            "pub fn production_reader() {}\n"
            "#[cfg(test)]\nmod fixtures {\n"
            "    use std::fs::OpenOptions;\n"
            "    const UNBALANCED_BRACE: &str = \"{\";\n"
            "    const CLOSING_BRACE: char = '}';\n"
            "    fn rewrite_fixture() { let _ = OpenOptions::new(); }\n"
            "}\n",
            encoding="utf-8",
        )
        self.validate()

    def test_production_open_options_is_rejected_even_with_inline_test_fixture(self) -> None:
        (self.root / "provider.rs").write_text(
            "use std::fs::OpenOptions;\n"
            "pub fn production_writer() { let _ = OpenOptions::new(); }\n"
            "#[cfg(test)]\nmod fixtures {\n"
            "    use std::fs::OpenOptions;\n"
            "    fn rewrite_fixture() { let _ = OpenOptions::new(); }\n"
            "}\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(BoundaryError, "write-capable API"):
            self.validate()

    def test_aliased_build_dependency_is_rejected(self) -> None:
        self.append_manifest(
            "\n[build-dependencies]\n"
            'hidden_capture = { package = "ctx-history-capture", version = "1" }\n'
        )
        with self.assertRaisesRegex(BoundaryError, "ctx-history-capture"):
            self.validate()

    def test_aliased_target_dependency_is_rejected(self) -> None:
        self.append_manifest(
            "\n[target.'cfg(unix)'.dependencies]\n"
            'hidden_index = { package = "ctx-history-index", version = "1" }\n'
        )
        with self.assertRaisesRegex(BoundaryError, "ctx-history-index"):
            self.validate()

    def test_aliased_target_dev_dependency_is_rejected(self) -> None:
        self.append_manifest(
            "\n[target.'cfg(test)'.dev-dependencies]\n"
            'hidden_capture = { package = "ctx-history-capture", version = "1" }\n'
        )
        with self.assertRaisesRegex(BoundaryError, "ctx-history-capture"):
            self.validate()

    def test_aliased_target_build_dependency_is_rejected(self) -> None:
        self.append_manifest(
            "\n[target.'cfg(windows)'.build-dependencies]\n"
            'hidden_index = { package = "ctx-history-index", version = "1" }\n'
        )
        with self.assertRaisesRegex(BoundaryError, "ctx-history-index"):
            self.validate()


if __name__ == "__main__":
    unittest.main()
