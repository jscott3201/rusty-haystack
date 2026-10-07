"""Reproduce Maturin's relocated project root with the original nested stub."""
import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest

HELPERS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HELPERS))
try:
    spec = importlib.util.spec_from_file_location("source_artifact_builder", HELPERS / "build.py")
    builder = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(builder)
finally:
    sys.path.pop(0)


class SourceTypingRepairTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="source typing archive ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / "archive"; self.root.mkdir()
        (self.root / "pyproject.toml").write_text('[tool.maturin]\nmanifest-path="rusty-haystack/Cargo.toml"\n')
        nested = self.root / "rusty-haystack"; nested.mkdir()
        (nested / "Cargo.toml").write_text('[package]\nname="rusty-haystack"\nversion="0.9.0"\n')
        self.expected = Path(self.temp.name) / "current-source.pyi"
        self.expected.write_bytes(b'class CurrentAPI: ...\n')
        self.nested = nested / "rusty_haystack.pyi"
        self.nested.write_bytes(self.expected.read_bytes())
        self.output = self.root / "rusty_haystack.pyi"

    def test_misplaced_stub_is_copied_without_regeneration(self):
        expected = builder.a.digest(self.expected)
        self.assertFalse(self.output.exists())
        self.assertEqual(builder.prepare_source_typing(self.root, self.expected), expected)
        self.assertEqual(self.output.read_bytes(), self.expected.read_bytes())
        self.assertEqual(self.nested.read_bytes(), self.expected.read_bytes())
        self.assertFalse((self.root / "py.typed").exists())  # Maturin emits it in the wheel.
        self.assertEqual(builder.prepare_source_typing(self.root, self.expected), expected)

    def test_missing_or_stale_nested_source_is_rejected(self):
        for stale in (None, b'class OldAPI: ...\n'):
            with self.subTest(stale=stale):
                if stale is None: self.nested.unlink()
                else: self.nested.write_bytes(stale)
                with self.assertRaisesRegex(builder.a.Invalid, "typing"):
                    builder.prepare_source_typing(self.root, self.expected)
                self.assertFalse(self.output.exists())

    def test_conflicting_destination_is_never_overwritten(self):
        self.output.write_bytes(b'class ConflictingAPI: ...\n')
        with self.assertRaisesRegex(builder.a.Invalid, "conflicting"):
            builder.prepare_source_typing(self.root, self.expected)
        self.assertEqual(self.output.read_bytes(), b'class ConflictingAPI: ...\n')


if __name__ == '__main__':
    unittest.main()
