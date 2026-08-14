#!/usr/bin/env python3
import tempfile
import unittest
from pathlib import Path

from check_history_provider_claude_cursor_boundary import (
    FORBIDDEN,
    REQUIRED,
    BoundaryError,
    validate_pack,
)


class ClaudeCursorBoundaryMutationTests(unittest.TestCase):
    def test_capture_dependency_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            manifest = root / "Cargo.toml"
            manifest.write_text('[package]\nname = "ctx-history-provider-claude-cursor"\n[dependencies]\nctx-history-capture = "1"\n', encoding="utf-8")
            (root / "src").mkdir()
            (root / "src/lib.rs").write_text("", encoding="utf-8")
            build = root / "BUILD.bazel"
            build.write_text("", encoding="utf-8")
            with self.assertRaisesRegex(BoundaryError, "dependency inventory|forbidden"):
                validate_pack(manifest, build)

    def test_bazel_only_forbidden_dependencies_are_rejected(self) -> None:
        for dependency in sorted(FORBIDDEN):
            with self.subTest(dependency=dependency), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                manifest = root / "Cargo.toml"
                manifest.write_text(
                    '[package]\nname = "ctx-history-provider-claude-cursor"\n'
                    "[dependencies]\n"
                    + "".join(f'{item} = "1"\n' for item in sorted(REQUIRED)),
                    encoding="utf-8",
                )
                (root / "src").mkdir()
                (root / "src/lib.rs").write_text(
                    "pub fn claude_jsonl_adapter<B> pub fn cursor_jsonl_adapter<B> "
                    "ProviderJsonlRuntime<B> CaptureProvider::Claude CaptureProvider::Cursor",
                    encoding="utf-8",
                )
                build = root / "BUILD.bazel"
                build.write_text(
                    f'"//crates/{dependency}:lib"\n',
                    encoding="utf-8",
                )
                with self.assertRaisesRegex(
                    BoundaryError,
                    f"Claude/Cursor Bazel graph gained {dependency} authority",
                ):
                    validate_pack(manifest, build)


if __name__ == "__main__":
    unittest.main()
