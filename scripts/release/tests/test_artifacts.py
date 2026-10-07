"""Process-level negative cases for the release helper contract; no native builds."""
import copy
import hashlib
import io
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import unittest
import zipfile

REPO = Path(__file__).resolve().parents[3]
HELPER = REPO / "scripts/release/artifacts.py"
SOURCE = "a" * 40
TARGET = "x86_64-unknown-linux-gnu"
VERSION = tomllib.loads((REPO / "Cargo.toml").read_text())["workspace"]["package"]["version"]
LOCK = (REPO / "Cargo.lock").read_bytes()


def sha(data):
    return hashlib.sha256(data).hexdigest()


def tar(path, files):
    with tarfile.open(path, "w:gz") as archive:
        for name, data in files.items():
            info = tarfile.TarInfo(name); info.size = len(data); info.mode = 0o755
            archive.addfile(info, io.BytesIO(data))


def receipt(path, kind, details):
    return {"schema": 1, "kind": kind,
            "source": {"revision": SOURCE, "dirty": False, "lock_sha256": sha(LOCK)},
            "package": {"name": "rusty-haystack-cli" if kind == "cli" else "rusty-haystack", "version": VERSION},
            "artifact": {"filename": path.name, "size": path.stat().st_size, "sha256": sha(path.read_bytes())},
            "build": {"target": "source" if kind == "sdist" else TARGET, "profile": "sdist" if kind == "sdist" else "release", "features": [] if kind == "cli" else ["pyo3/extension-module"], "tools": {"rustc": "rustc 1.99.0\nrelease: 1.99.0\nhost: " + TARGET, "cargo": "cargo 1.99.0", "target": "source" if kind == "sdist" else TARGET, "context": {"controlled_environment": {"CARGO_BUILD_JOBS": "2", "CARGO_INCREMENTAL": "0", "RUSTUP_TOOLCHAIN": "1.99.0"}}, "executables": {name: {"path": "/builder/"+name, "sha256": "e"*64} for name in ("cargo", "rustc", "rustdoc")}, "maturin": "maturin 1.15.0", "python": {"implementation": "CPython", "cache_tag": "cpython-312", "version": "3.12.13", "platform": "linux-x86_64"}}},
            "details": details, **({"removed_lock_packages": []} if kind == "sdist" else {})}


class ArtifactCLI(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="haystack artifact tests ")
        self.root = Path(self.temp.name)
        self.incoming = self.root / "download"
        self.evidence = self.root / "evidence"; self.evidence.mkdir()
        elf = bytearray(64); elf[:4] = b"\x7fELF"; elf[4:6] = b"\x02\x01"; elf[18:20] = (62).to_bytes(2, "little")
        self.paths = {}; self.receipts = {}
        for kind in ("cli", "wheel", "sdist"):
            bundle = self.incoming / ("candidate-" + kind)
            files = bundle / "files"; files.mkdir(parents=True)
            (bundle / "receipts").mkdir()
            if kind == "cli":
                path = files / f"haystack-{VERSION}-{TARGET}.tar.gz"
                tar(path, {"haystack": bytes(elf), "LICENSE": b"MIT"}); details = None
            elif kind == "wheel":
                path = files / f"rusty_haystack-{VERSION}-cp312-cp312-manylinux_2_17_x86_64.whl"
                with zipfile.ZipFile(path, "w") as archive:
                    archive.writestr(f"rusty_haystack-{VERSION}.dist-info/METADATA", f"Name: rusty-haystack\nVersion: {VERSION}\nRequires-Python: >=3.11\n")
                    archive.writestr(f"rusty_haystack-{VERSION}.dist-info/WHEEL", "Wheel-Version: 1.0\nGenerator: maturin (1.15.0)\nRoot-Is-Purelib: false\nTag: cp312-cp312-manylinux_2_17_x86_64\n")
                    archive.writestr("rusty_haystack/rusty_haystack.cpython-312-x86_64-linux-gnu.so", elf)
                    archive.writestr("rusty_haystack/__init__.pyi", "class Number: ...\n")
                    archive.writestr("rusty_haystack/py.typed", "")
                details = {"python": "cp312", "abi": "cp312", "platforms": ["manylinux_2_17_x86_64"], "requires_python": ">=3.11"}
            else:
                path = files / f"rusty_haystack-{VERSION}.tar.gz"
                tar(path, {f"rusty_haystack-{VERSION}/Cargo.lock": LOCK, f"rusty_haystack-{VERSION}/PKG-INFO": f"Name: rusty-haystack\nVersion: {VERSION}\n".encode()})
                details = {"lock_sha256": sha(LOCK)}
            record = receipt(path, kind, details)
            record_path = bundle / "receipts" / (path.name + ".json")
            record_path.write_text(json.dumps(record))
            self.paths[kind] = path; self.receipts[kind] = record_path
            checks = ["provenance", "archive-metadata"]
            checks += ["cli-version", "cli-fixture"] if kind == "cli" else ["installed-smoke", "typing-layout"]
            if kind == "sdist": checks += ["locked-source-metadata", "pep517-install"]
            (self.evidence / (path.name + ".json")).write_text(json.dumps({"schema": 1, "status": "passed", "artifact": record["artifact"], "receipt_sha256": sha(record_path.read_bytes()), "checks": checks, "environment": {"host": TARGET}}))

    def tearDown(self):
        self.temp.cleanup()

    def invoke(self, *arguments, success=True):
        result = subprocess.run([sys.executable, str(HELPER), *map(str, arguments)], capture_output=True, text=True, cwd=self.root)
        self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
        return json.loads(result.stdout if success else result.stderr)

    def verify(self, success=True):
        return self.invoke("verify", "--repo", REPO, "--input", self.incoming, "--source", SOURCE, "--profile", "native", "--target", TARGET, success=success)

    def stage(self, success=True):
        return self.invoke("stage", "--repo", REPO, "--input", self.incoming, "--source", SOURCE, "--profile", "native", "--target", TARGET, "--evidence", self.evidence, "--output", self.root / "publication", success=success)

    def mutate_receipt(self, kind, operation):
        path = self.receipts[kind]; record = json.loads(path.read_text()); operation(record); path.write_text(json.dumps(record))

    def test_verified_files_stage_identical_bytes_without_receipts(self):
        self.assertEqual(self.verify()["status"], "verified")
        staged = self.stage()
        self.assertEqual(staged["status"], "staged")
        staged_record = self.root / "stage.json"; staged_record.write_text(json.dumps(staged))
        self.invoke("check-staged", "--repo", REPO, "--input", self.root / "publication", "--receipt", staged_record, "--source", SOURCE, "--profile", "native", "--target", TARGET)
        self.assertEqual({p.name for p in (self.root / "publication").iterdir()}, {p.name for p in self.paths.values()})
        for path in self.paths.values(): self.assertEqual((self.root / "publication" / path.name).read_bytes(), path.read_bytes())
        self.stage(success=False)  # Never overwrite an uncertain prior stage.

    def test_staged_download_rejects_missing_changed_and_extra_files(self):
        staged = self.stage()
        record_path = self.root / "stage.json"; record_path.write_text(json.dumps(staged))
        def check():
            return self.invoke("check-staged", "--repo", REPO, "--input", self.root / "publication", "--receipt", record_path, "--source", SOURCE, "--profile", "native", "--target", TARGET, success=False)
        file = self.root / "publication" / self.paths["wheel"].name
        original = file.read_bytes()
        file.write_bytes(original + b"changed after upload"); check()
        file.unlink(); check()
        omitted = copy.deepcopy(staged)
        omitted["files"] = [item for item in omitted["files"] if item["filename"] != file.name]
        record_path.write_text(json.dumps(omitted)); check()
        record_path.write_text(json.dumps(staged)); file.write_bytes(original)
        (file.parent / "receipt.json").write_text("{}"); check()

    def test_digest_tampering(self):
        with self.paths["cli"].open("ab") as stream: stream.write(b"tampered")
        self.assertIn("digest", self.verify(success=False)["error"])

    def test_wrong_source_version_target_and_abi(self):
        changes = [("cli", lambda r: r["source"].update(revision="b" * 40)), ("wheel", lambda r: r["package"].update(version="99.0.0")), ("cli", lambda r: r["build"].update(target="aarch64-unknown-linux-gnu")), ("wheel", lambda r: r["details"].update(python="cp313"))]
        for kind, operation in changes:
            with self.subTest(kind=kind, operation=operation):
                original = self.receipts[kind].read_bytes()
                self.mutate_receipt(kind, operation); self.verify(success=False)
                self.receipts[kind].write_bytes(original)

    def test_missing_duplicate_and_unexpected_inventory(self):
        missing = self.receipts["cli"]; data = missing.read_bytes(); missing.unlink()
        self.verify(success=False); missing.write_bytes(data)
        duplicate = self.incoming / "duplicate"; shutil.copytree(self.incoming / "candidate-cli", duplicate)
        self.verify(success=False); shutil.rmtree(duplicate)
        extra = self.paths["wheel"].parent / "unexpected.whl"; extra.write_bytes(b"extra")
        self.verify(success=False)

    def test_missing_duplicate_and_unbound_evidence(self):
        path = self.evidence / (self.paths["cli"].name + ".json"); original = path.read_bytes()
        path.unlink(); self.stage(success=False); path.write_bytes(original)
        record = json.loads(original); record["checks"].remove("cli-fixture"); path.write_text(json.dumps(record)); self.stage(success=False)
        record = json.loads(original); record["checks"].append("cli-fixture"); path.write_text(json.dumps(record)); self.stage(success=False)
        record = json.loads(original); record["receipt_sha256"] = "0" * 64; path.write_text(json.dumps(record)); self.stage(success=False)
        record = json.loads(original); record["environment"]["host"] = "aarch64-apple-darwin"; path.write_text(json.dumps(record)); self.stage(success=False)
        path.write_text('{"schema":1,"schema":1}'); self.stage(success=False)
        self.assertFalse((self.root / "publication").exists())

    def test_unsafe_archive_paths_rejected_before_extraction(self):
        path = self.paths["cli"]
        for name in ("../escape", "/absolute", "C:/windows", "nested\\escape"):
            with self.subTest(name=name):
                tar(path, {name: b"bad"})
                self.mutate_receipt("cli", lambda r: r.update(artifact={"filename": path.name, "size": path.stat().st_size, "sha256": sha(path.read_bytes())}))
                self.assertIn("unsafe archive path", self.verify(success=False)["error"])
                self.assertFalse((self.root / "escape").exists())

    def test_tag_guard_uses_parsed_version(self):
        self.assertEqual(self.invoke("version", "--repo", REPO, "--tag", "v" + VERSION), {"version": VERSION})
        self.invoke("version", "--repo", REPO, "--tag", "v99.0.0", success=False)

    def test_lock_pruning_removes_only_existing_dependency_edges(self):
        # The observed sdist keeps clap/clap_builder identities while dropping
        # CLI-only clap_derive/strsim edges and an anstream edge to a retained node.
        before = self.root / "before.lock"; after = self.root / "after.lock"
        initial = ('version = 4\n[[package]]\nname="clap"\nversion="4.6.7"\nchecksum="same"\n'
                   'dependencies=["clap_builder", "clap_derive"]\n'
                   '[[package]]\nname="clap_builder"\nversion="4.6.7"\nchecksum="builder"\n'
                   'dependencies=["anstream", "strsim"]\n'
                   '[[package]]\nname="anstream"\nversion="1.0.0"\n'
                   '[[package]]\nname="clap_derive"\nversion="4.6.7"\n'
                   '[[package]]\nname="strsim"\nversion="0.11.1"\n')
        pruned = ('version = 4\n[[package]]\nname="clap"\nversion="4.6.7"\nchecksum="same"\n'
                  'dependencies=["clap_builder"]\n'
                  '[[package]]\nname="clap_builder"\nversion="4.6.7"\nchecksum="builder"\n'
                  '[[package]]\nname="anstream"\nversion="1.0.0"\n')
        before.write_text(initial); after.write_text(pruned)
        removed = self.invoke("check-lock-prune", "--before", before, "--after", after)["removed"]
        self.assertEqual(removed, [["clap_derive", "4.6.7", ""], ["strsim", "0.11.1", ""]])
        for changed in (pruned.replace('["clap_builder"]', '["clap_builder", "anstream"]'),
                        pruned.replace('checksum="same"', 'checksum="changed"'),
                        pruned.replace('version="4.6.7"', 'version="4.6.8"', 1)):
            after.write_text(changed)
            self.invoke("check-lock-prune", "--before", before, "--after", after, success=False)

    def test_lock_pruning_allows_only_unreferenced_removal(self):
        before = self.root / "before.lock"; after = self.root / "after.lock"
        keep = 'version = 4\n[[package]]\nname="app"\nversion="1.0.0"\ndependencies=["lib"]\n[[package]]\nname="lib"\nversion="1.0.0"\nchecksum="old"\n'
        unused = '[[package]]\nname="unused"\nversion="1.0.0"\n'
        before.write_text(keep + unused); after.write_text(keep)
        self.assertEqual(self.invoke("check-lock-prune", "--before", before, "--after", after)["removed"], [["unused", "1.0.0", ""]])
        for bad in (keep + unused + '[[package]]\nname="added"\nversion="1.0.0"\n', keep.replace('checksum="old"', 'checksum="new"'), keep.replace('name="lib"\nversion="1.0.0"', 'name="lib"\nversion="2.0.0"'), keep.split('[[package]]\nname="lib"')[0]):
            after.write_text(bad); self.invoke("check-lock-prune", "--before", before, "--after", after, success=False)


if __name__ == "__main__":
    unittest.main()
