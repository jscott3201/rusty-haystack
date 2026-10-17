"""Selected interpreter/ABI and execution-mode admission controls."""
import copy
import hashlib
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

RELEASE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(RELEASE))
import artifacts as a
import runtime_profile as runtime
import selected_wheel as selected
sys.path.pop(0)


class SelectedWheelTests(unittest.TestCase):
    def wheel(self, root, interpreter="cp314", abi="cp314t"):
        path = root / f"rusty_haystack-0.9.0-{interpreter}-{abi}-manylinux_2_17_x86_64.whl"
        elf = bytearray(64); elf[:4] = b"\x7fELF"; elf[4:6] = b"\x02\x01"; elf[18:20] = (62).to_bytes(2, "little")
        with zipfile.ZipFile(path, "w") as archive:
            archive.writestr("rusty_haystack-0.9.0.dist-info/METADATA", "Name: rusty-haystack\nVersion: 0.9.0\nRequires-Python: >=3.11\n")
            archive.writestr("rusty_haystack-0.9.0.dist-info/WHEEL", f"Generator: maturin (1.15.0)\nRoot-Is-Purelib: false\nTag: {interpreter}-{abi}-manylinux_2_17_x86_64\n")
            archive.writestr("rusty_haystack/rusty_haystack.cpython-314t-x86_64-linux-gnu.so", elf)
            archive.writestr("rusty_haystack/__init__.py", "from .rusty_haystack import *\n")
            for name in a.TYPING_FILES:
                archive.writestr("rusty_haystack/" + name, "")
        return path

    def test_free_threaded_interpreter_and_abi_are_distinct(self):
        with tempfile.TemporaryDirectory() as directory:
            path = self.wheel(Path(directory))
            _, details = a.inspect_file(path, "wheel", "0.9.0", "x86_64-unknown-linux-gnu", "cp314t")
            self.assertEqual((details["python"], details["abi"]), ("cp314", "cp314t"))

    def test_wrong_free_threaded_abi_and_unselected_platform_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path = self.wheel(Path(directory), abi="cp314")
            with self.assertRaises(a.Invalid):
                a.inspect_file(path, "wheel", "0.9.0", "x86_64-unknown-linux-gnu", "cp314t")
            path = self.wheel(Path(directory))
            with self.assertRaises(a.Invalid):
                a.inspect_file(path, "wheel", "0.9.0", "aarch64-unknown-linux-gnu", "cp314t")

    def test_selected_cells_do_not_expand_release_inventory(self):
        self.assertEqual(a.PYTHONS, ("cp311", "cp312", "cp313"))
        self.assertEqual(len(a.expected_tuples("python")), 13)

    def test_archived_bytecode_is_rejected_before_installation(self):
        with tempfile.TemporaryDirectory() as directory:
            path = self.wheel(Path(directory))
            with zipfile.ZipFile(path, "a") as archive:
                archive.writestr("rusty_haystack/__pycache__/__init__.cpython-312.pyc", b"builder-specific cache")
            with self.assertRaises(a.Invalid):
                a.inspect_file(path, "wheel", "0.9.0", "x86_64-unknown-linux-gnu", "cp314t")

    def observed(self):
        return {"implementation": "CPython", "version": "3.14.4", "executable": "/env/bin/python",
                "cache_tag": "cpython-314", "platform": "linux-x86_64", "machine": "x86_64",
                "soabi": "cpython-314t-x86_64-linux-gnu", "ext_suffix": ".cpython-314t-x86_64-linux-gnu.so",
                "py_gil_disabled": True, "gil_enabled": False, "gil_env": None, "gil_xoption": None}

    def test_runtime_rejects_enabled_missing_forced_and_wrong_abi_observations(self):
        observation = self.observed()
        runtime.validate(observation, "cp314t")
        for key, value in (("gil_enabled", True), ("gil_enabled", None), ("gil_env", "0"), ("gil_xoption", "0"),
                           ("py_gil_disabled", False), ("soabi", "cpython-314-x86_64-linux-gnu"),
                           ("ext_suffix", ".cpython-314-x86_64-linux-gnu.so"), ("cache_tag", "cpython-312"), ("version", "3.15.0")):
            with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                runtime.validate(observation | {key: value}, "cp314t")
        with self.assertRaises(ValueError):
            runtime.validate(observation, "cp312")
        ordinary = observation | {"version": "3.12.13", "cache_tag": "cpython-312", "py_gil_disabled": False,
                                  "gil_enabled": None, "soabi": "cpython-312-x86_64-linux-gnu", "ext_suffix": ".cpython-312-x86_64-linux-gnu.so"}
        runtime.validate(ordinary, "cp312")
        selected.selected_interpreter("aarch64-apple-darwin", "cp314t", observation | {"machine": "arm64"})
        with self.assertRaises(ValueError):
            selected.selected_interpreter("aarch64-apple-darwin", "cp314t", observation | {"machine": "arm64", "version": "3.14.5"})
        selected.selected_interpreter("x86_64-unknown-linux-gnu", "cp314t", observation | {"version": "3.14.5"})

    def evidence(self, root):
        path = self.wheel(root)
        _, details = a.inspect_file(path, "wheel", "0.9.0", "x86_64-unknown-linux-gnu", "cp314t")
        observed = self.observed()
        receipt = {"source": {"revision": "a" * 40, "dirty": False, "lock_sha256": "b" * 64},
                   "artifact": {"filename": path.name, "size": path.stat().st_size, "sha256": a.digest(path)},
                   "package": {"version": "0.9.0"}, "build": {"target": "x86_64-unknown-linux-gnu", "tools": {"python": observed}}, "details": details}
        receipt_path = root / "receipt.json"; receipt_path.write_text(json.dumps(receipt))
        fixture = {"sha256": "c" * 64}
        with zipfile.ZipFile(path) as archive:
            members = {name: hashlib.sha256(archive.read(name)).hexdigest() for name in archive.namelist() if name.startswith("rusty_haystack/")}
        installed = {"wheel_members_sha256": members, "typing_files_sha256": details["typing_files_sha256"],
                     "version": "0.9.0", "python": "3.14.4", "prefix": "/env", "module_origin": "/env/lib/rusty_haystack/__init__.py",
                     "native_origin": "/env/lib/rusty_haystack/rusty_haystack.cpython-314t-x86_64-linux-gnu.so", "runtime_before": observed, "runtime_after": observed}
        phases = ["before-package", *("import:" + name for name in selected.IMPORTS), *("dependency:" + name for name in selected.DEPENDENCIES), "after-consumer"]
        result = {"abi": "cp314t", "wheel_sha256": a.digest(path), "installed": installed, "installed_after": copy.deepcopy(installed),
                  "runtime_modes": {name: {"status": "passed", "abi": "cp314t", "pid": 10,
                                           "script": "positive.py" if name == "positive-runtime" else name + ".py",
                                           "observations": [{"phase": phase, "runtime": copy.deepcopy(observed)} for phase in phases]} for name in selected.PROCESSES},
                  **{section: dict.fromkeys(flags, True) for section, flags in selected.REQUIRED_FLAGS.items()},
                  "checker": "pyright 1.1.413", "positive_errors": 0, "negative_lines_rejected": 79, "watch_fixture_build": fixture}
        result["concurrency_runtime"].update(shared_graph_final_count=201, create_destroy_cycles=12,
                                             **dict.fromkeys(("source_dict_constructor", "source_dict_factory", "source_list"), {"conversions": 100, "mutations": 1}))
        result["watch_runtime"]["fixture_sha256"] = fixture["sha256"]
        evidence = {"schema": 1, "profile": "selected-wheel", "status": "passed", "source": receipt["source"], "target": receipt["build"]["target"],
                    "artifact": receipt["artifact"], "receipt_sha256": a.digest(receipt_path), "capabilities": selected.capabilities(),
                    "fixture_receipt_sha256": "e" * 64,
                    "native_linkage": {"status": "passed", "format": "ELF", "minimum_glibc": "2.17", "dependencies": ["libc.so.6"],
                                       "native_sha256": members["rusty_haystack/rusty_haystack.cpython-314t-x86_64-linux-gnu.so"]}, "consumer": result}
        return evidence, receipt, receipt_path, path, fixture

    def test_digest_source_capability_mode_and_fixture_mutations_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            evidence, receipt, receipt_path, path, fixture = self.evidence(Path(directory))
            selected.verify_evidence(evidence, receipt, receipt_path, path, fixture, "e" * 64)
            mutations = (
                lambda e: e.update(receipt_sha256="d" * 64),
                lambda e: e.update(fixture_receipt_sha256="d" * 64),
                lambda e: e["source"].update(revision="d" * 40),
                lambda e: e["capabilities"].pop("bounded-watches"),
                lambda e: e["capabilities"].update(postgresql={"status": "passed"}),
                lambda e: e["consumer"].update(wheel_sha256="d" * 64),
                lambda e: e["consumer"]["installed"]["wheel_members_sha256"].update({"rusty_haystack/__init__.py": "d" * 64}),
                lambda e: e["consumer"]["installed"].update(module_origin="/checkout/rusty_haystack/__init__.py"),
                lambda e: e["consumer"]["runtime_modes"].pop("concurrency-runtime"),
                lambda e: e["consumer"]["runtime_modes"]["sync-runtime"]["observations"].pop(),
                lambda e: e["consumer"]["runtime_modes"]["watch-runtime"]["observations"][-1]["runtime"].update(gil_enabled=True),
                lambda e: e["consumer"]["async_runtime"].update(closed_owners=False),
                lambda e: e["consumer"]["concurrency_runtime"]["source_dict_constructor"].update(mutations=0),
                lambda e: e["consumer"]["watch_runtime"].update(fixture_sha256="d" * 64),
                lambda e: e["native_linkage"].update(status="unselected"),
            )
            for index, mutation in enumerate(mutations):
                changed = copy.deepcopy(evidence); mutation(changed)
                with self.subTest(mutation=index), self.assertRaises(ValueError):
                    selected.verify_evidence(changed, receipt, receipt_path, path, fixture, "e" * 64)

    def test_selected_download_ids_reject_partial_or_failed_producers_and_reuse_ids(self):
        needs = {"build": {"result": "success", "outputs": {"cli": "101", "wheel": "102", "sdist": "103"}},
                 "build-ft": {"result": "success", "outputs": {"wheel_ft": "104"}}}
        self.assertEqual(a.download_ids("selected-wheels", needs)["wheel_ft"], "104")
        for changed in (needs | {"build-ft": {"result": "failure", "outputs": {"wheel_ft": "104"}}},
                        needs | {"build-ft": {"result": "success", "outputs": {"wheel_ft": "102"}}},
                        {"build": needs["build"]}):
            with self.assertRaises(a.Invalid):
                a.download_ids("selected-wheels", changed)

    def test_native_linkage_rejects_foreign_libraries_and_understated_minimums(self):
        with tempfile.TemporaryDirectory() as directory:
            native = Path(directory) / "native.so"; native.write_bytes(b"native")
            libraries = str(native) + ":\n\t@rpath/native.so (compatibility version 0.0.0)\n\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0)"
            commands = "cmd LC_BUILD_VERSION\n cmdsize 32\n platform 1\n minos 11.0\n sdk 27.0\n"
            with patch.object(a, "native_identity", return_value=("darwin", "aarch64")):
                with patch.object(a, "output", side_effect=[libraries, commands]):
                    self.assertEqual(selected.native_linkage(native, "aarch64-apple-darwin", ["macosx_11_0_arm64"])["minimum_macos"], "11.0")
                for changed, changed_commands, tag in ((libraries.replace("/usr/lib/libSystem.B.dylib", "/tmp/libunexpected.dylib"), commands, "macosx_11_0_arm64"),
                                                       (libraries.replace("libSystem.B.dylib", "libpython3.14t.dylib"), commands, "macosx_11_0_arm64"),
                                                       (libraries, commands.replace("minos 11.0", "minos 12.0"), "macosx_11_0_arm64"),
                                                       (libraries, commands + "cmd LC_RPATH\n cmdsize 40\n path /tmp (offset 12)", "macosx_11_0_arm64")):
                    with patch.object(a, "output", side_effect=[changed, changed_commands]), self.assertRaises(a.Invalid):
                        selected.native_linkage(native, "aarch64-apple-darwin", [tag])
            dynamic = "0x1 (NEEDED) Shared library: [libc.so.6]"
            versions = "Name: GLIBC_2.17\nName: GLIBC_2.34"
            with patch.object(a, "native_identity", return_value=("linux", "x86_64")):
                with patch.object(a, "output", side_effect=[dynamic, versions]):
                    self.assertEqual(selected.native_linkage(native, "x86_64-unknown-linux-gnu", ["manylinux_2_34_x86_64"])["minimum_glibc"], "2.34")
                with patch.object(a, "output", side_effect=[dynamic, versions]), self.assertRaises(a.Invalid):
                    selected.native_linkage(native, "x86_64-unknown-linux-gnu", ["manylinux_2_17_x86_64"])

    def test_native_linkage_checks_legacy_manylinux_floors(self):
        with tempfile.TemporaryDirectory() as directory:
            native = Path(directory) / "native.so"; native.write_bytes(b"native")
            dynamic = "0x1 (NEEDED) Shared library: [libc.so.6]"
            with patch.object(a, "native_identity", return_value=("linux", "x86_64")):
                for tag, floor, newer in (("manylinux1_x86_64", "2.5", "2.6"),
                                           ("manylinux2010_x86_64", "2.12", "2.13"),
                                           ("manylinux2014_x86_64", "2.17", "2.18")):
                    with self.subTest(tag=tag, requirement=floor), patch.object(a, "output", side_effect=[dynamic, "Name: GLIBC_" + floor]):
                        self.assertEqual(selected.native_linkage(native, "x86_64-unknown-linux-gnu", [tag])["minimum_glibc"], floor)
                    for requirement in (newer, "2.34"):
                        with self.subTest(tag=tag, requirement=requirement), patch.object(a, "output", side_effect=[dynamic, "Name: GLIBC_" + requirement]), self.assertRaises(a.Invalid):
                            selected.native_linkage(native, "x86_64-unknown-linux-gnu", [tag])
                with patch.object(a, "output", side_effect=[dynamic, "Name: GLIBC_2.34"]), self.assertRaises(a.Invalid):
                    selected.native_linkage(native, "x86_64-unknown-linux-gnu", ["manylinux_2_34_x86_64", "manylinux2014_x86_64"])

    def test_native_linkage_rejects_unknown_manylinux_and_preserves_native_linux(self):
        with tempfile.TemporaryDirectory() as directory:
            native = Path(directory) / "native.so"; native.write_bytes(b"native")
            dynamic = "0x1 (NEEDED) Shared library: [libc.so.6]"
            versions = "Name: GLIBC_2.34"
            with patch.object(a, "native_identity", return_value=("linux", "x86_64")):
                for tag in ("manylinux2099_x86_64", "manylinux_2_x86_64", "manylinux_2_34_1_x86_64"):
                    with self.subTest(tag=tag), patch.object(a, "output", side_effect=[dynamic, versions]), self.assertRaises(a.Invalid):
                        selected.native_linkage(native, "x86_64-unknown-linux-gnu", [tag])
                for tag in ("manylinux_2_34_x86_64", "manylinux_2_35_x86_64", "linux_x86_64"):
                    with self.subTest(tag=tag), patch.object(a, "output", side_effect=[dynamic, versions]):
                        self.assertEqual(selected.native_linkage(native, "x86_64-unknown-linux-gnu", [tag])["minimum_glibc"], "2.34")


if __name__ == "__main__":
    unittest.main()
