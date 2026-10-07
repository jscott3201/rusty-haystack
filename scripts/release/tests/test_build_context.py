"""Process regressions for actual compiler selection and repository-relative output."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[3]
BUILD = REPO / "scripts/release/build.py"
SOURCE = "a" * 40
TARGET = "x86_64-unknown-linux-gnu"


class BuildContextTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="artifact build context ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.repo = self.root / "repo"; self.repo.mkdir()
        self.caller = self.root / "caller"; self.caller.mkdir()
        self.bin = self.root / "bin"; self.bin.mkdir()
        self.cargo_home = self.root / "cargo-home"; self.cargo_home.mkdir()
        (self.repo / "Cargo.toml").write_text('[workspace.package]\nversion="0.9.0"\n')
        (self.repo / "Cargo.lock").write_text('version=4\n[[package]]\nname="test"\nversion="0.9.0"\n')
        (self.repo / "LICENSE").write_text("MIT")
        for crate in ("haystack-cli", "rusty-haystack"):
            directory = self.repo / crate; directory.mkdir()
            (directory / "Cargo.toml").write_text('[package]\nname="'+crate+'"\nversion.workspace=true\n')
        (self.repo / "rusty-haystack/pyproject.toml").write_text('[project]\nname="rusty-haystack"\ndynamic=["version"]\n')
        self.program("git", "import sys\nprint('"+SOURCE+"' if 'rev-parse' in sys.argv else '')\n")
        self.program("rustc", "print('rustc 1.99.0\\nhost: "+TARGET+"\\nrelease: 1.99.0')\n")
        self.program("rustdoc", "print('rustdoc 1.99.0')\n")
        self.program("other-rustc", "print('rustc 1.98.0\\nhost: "+TARGET+"\\nrelease: 1.98.0')\n")
        self.program("rustup", "from pathlib import Path\nimport sys\nprint(Path(__file__).parent / sys.argv[-1])\n")
        self.program("cargo", '''import os,sys
from pathlib import Path
if '--version' in sys.argv:
 print('cargo 1.99.0')
else:
 path=Path(os.environ.get('CARGO_TARGET_DIR','target')) / 'x86_64-unknown-linux-gnu/release/haystack'
 path.parent.mkdir(parents=True,exist_ok=True)
 data=bytearray(64); data[:4]=b'\\x7fELF'; data[4:6]=b'\\x02\\x01'; data[18:20]=(62).to_bytes(2,'little')
 path.write_bytes(bytes(data)+b'newly-built-in-repo')
''')
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith(("CARGO_", "RUSTC", "RUSTDOC", "RUSTFLAGS", "RUSTUP_TOOLCHAIN"))}
        self.env.update(PATH=str(self.bin)+os.pathsep+self.env["PATH"], CARGO_HOME=str(self.cargo_home))

    def program(self, name, body):
        path = self.bin / name
        path.write_text("#!"+sys.executable+"\n"+body)
        path.chmod(0o755)
        return path

    def invoke(self, **overrides):
        return subprocess.run([sys.executable, str(BUILD), "cli", "--repo", str(self.repo),
                               "--source", SOURCE, "--target", TARGET,
                               "--bundle", str(self.root / "candidate"), "--work", str(self.root / "work")],
                              cwd=self.caller, env=self.env | overrides, text=True,
                              capture_output=True, timeout=30)

    def test_relative_target_directory_never_seals_stale_caller_binary(self):
        stale = self.caller / "relative-target" / TARGET / "release/haystack"
        stale.parent.mkdir(parents=True)
        data=bytearray(64); data[:4]=b'\x7fELF'; data[4:6]=b'\x02\x01'; data[18:20]=(62).to_bytes(2,'little')
        stale.write_bytes(bytes(data)+b'stale-caller-binary')
        result = self.invoke(CARGO_TARGET_DIR="relative-target")
        self.assertEqual(result.returncode, 0, result.stdout+result.stderr)
        record=json.loads(result.stdout)
        path=self.root / "candidate/files" / record["artifact"]["filename"]
        with tarfile.open(path) as archive:
            shipped=archive.extractfile('haystack').read()
        self.assertTrue(shipped.endswith(b'newly-built-in-repo'))
        self.assertNotEqual(shipped, stale.read_bytes())
        receipt=json.loads(next((self.root/'candidate/receipts').iterdir()).read_text())
        self.assertEqual(receipt['artifact']['sha256'],hashlib.sha256(path.read_bytes()).hexdigest())

    def test_different_selected_compiler_cannot_receive_successful_provenance(self):
        result=self.invoke(RUSTC=str(self.bin/'other-rustc'))
        self.assertNotEqual(result.returncode,0,result.stdout)
        self.assertIn('RUSTC',result.stderr)
        self.assertFalse((self.root/'candidate/receipts').exists())

    def test_hidden_profile_flags_and_toolchain_selectors_are_rejected(self):
        import shutil
        for name in ("CARGO_PROFILE_RELEASE_OPT_LEVEL", "CARGO_BUILD_RUSTC", "RUSTFLAGS", "RUSTC_WRAPPER", "RUSTUP_TOOLCHAIN"):
            with self.subTest(name=name):
                result=self.invoke(**{name: "private-unrecorded-selector"})
                self.assertNotEqual(result.returncode,0,result.stdout)
                self.assertIn(name,result.stderr)
                self.assertNotIn("private-unrecorded-selector",result.stdout+result.stderr)
                self.assertFalse((self.root/'candidate/receipts').exists())
                for path in (self.root/'candidate',self.root/'work'):
                    if path.exists(): shutil.rmtree(path)

    def test_cargo_compiler_or_profile_configuration_is_rejected(self):
        config=self.repo/'.cargo'; config.mkdir()
        (config/'config.toml').write_text('[build]\nrustc="private-compiler-setting"\n')
        result=self.invoke()
        self.assertNotEqual(result.returncode,0,result.stdout)
        self.assertNotIn('private-compiler-setting',result.stdout+result.stderr)
        self.assertFalse((self.root/'candidate/receipts').exists())


if __name__ == '__main__':
    unittest.main()
