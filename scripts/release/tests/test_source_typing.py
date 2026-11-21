"""Verify the explicit Python package at Maturin's relocated project root."""
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


class SourceTypingLayoutTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="source typing archive ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / "archive"; self.root.mkdir()
        self.expected = Path(self.temp.name) / "current-package"; self.expected.mkdir()
        self.package = self.root / "rusty_haystack"; self.package.mkdir()
        for directory in (self.expected, self.package):
            (directory / "__init__.py").write_bytes(b"from .rusty_haystack import *\n")
            for name in builder.a.TYPING_FILES:
                (directory / name).write_bytes(b"" if name == "py.typed" else b"class CurrentAPI: ...\n")

    def test_relocated_package_is_verified_without_rewriting(self):
        expected = builder.a.typing_hashes(self.expected)
        self.assertEqual(builder.prepare_source_typing(self.root, self.expected), expected)
        self.assertEqual(builder.a.typing_hashes(self.package), expected)
        self.assertFalse((self.root / "rusty_haystack.pyi").exists())
        self.assertEqual(builder.prepare_source_typing(self.root, self.expected), expected)

    def test_missing_or_stale_file_is_rejected(self):
        for name in ("__init__.pyi", "kinds.pyi", "py.typed"):
            original = (self.package / name).read_bytes()
            for stale in (None, b"class OldAPI: ...\n"):
                with self.subTest(name=name, stale=stale):
                    if stale is None: (self.package / name).unlink()
                    else: (self.package / name).write_bytes(stale)
                    with self.assertRaisesRegex(builder.a.Invalid, "typing"):
                        builder.prepare_source_typing(self.root, self.expected)
                    (self.package / name).write_bytes(original)

    def test_unexpected_or_symlinked_stubs_and_stale_loader_are_rejected(self):
        extra = self.package / "unexpected.pyi"; extra.write_bytes(b"")
        with self.assertRaisesRegex(builder.a.Invalid, "typing"):
            builder.prepare_source_typing(self.root, self.expected)
        extra.unlink()
        target = self.package / "kinds.pyi"; target.unlink(); target.symlink_to(self.expected / "kinds.pyi")
        with self.assertRaisesRegex(builder.a.Invalid, "typing"):
            builder.prepare_source_typing(self.root, self.expected)
        target.unlink(); target.write_bytes((self.expected / "kinds.pyi").read_bytes())
        (self.package / "__init__.py").write_bytes(b"stale loader\n")
        with self.assertRaisesRegex(builder.a.Invalid, "loader mismatch"):
            builder.prepare_source_typing(self.root, self.expected)


if __name__ == '__main__':
    unittest.main()
