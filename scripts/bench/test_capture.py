"""Capture input admission tests; no Cargo builds or timings are executed."""
import importlib.util
import contextlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).with_name("capture.py")
SPEC = importlib.util.spec_from_file_location("capture", SCRIPT)
capture = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(capture)


class CaptureInputTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name).resolve()
        self.root = self.directory / "workspace" / "project"
        self.root.mkdir(parents=True)
        self.home = self.directory / "user"
        self.home.mkdir()
        self.environment = {"HOME": str(self.home), "USERPROFILE": str(self.home)}

    def test_clean_inputs_and_caller_environment_are_preserved(self):
        self.environment.update(RUSTFLAGS="caller flags", CARGO_BUILD_JOBS="16",
                                CARGO_INCREMENTAL="1", CARGO_TARGET_DIR="caller-target")
        before = self.environment.copy()
        self.assertEqual(capture.check_build_inputs(self.root, self.environment),
                         {"cargo_config_files": [], "workspace_profiles": {}})
        self.assertEqual(self.environment, before)

    def test_build_overrides_fail_without_disclosing_values(self):
        overrides = (
            "RUSTC", "RUSTDOC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER",
            "RUSTC_BOOTSTRAP", "RUST_TARGET_PATH", "RUSTUP_TOOLCHAIN",
            "CARGO_BUILD_RUSTC", "CARGO_BUILD_TARGET", "CARGO_BUILD_RUSTFLAGS",
            "CARGO_BUILD_BUILD_DIR", "CARGO_PROFILE_BENCH_OPT_LEVEL",
            "CARGO_ENCODED_RUSTFLAGS", "CARGO_ENCODED_RUSTDOCFLAGS",
            "CARGO_TARGET_X86_64_APPLE_DARWIN_RUNNER",
            "CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER", "CARGO_UNSTABLE_BUILD_STD",
            "CARGO_REGISTRY_TOKEN", "CC", "CFLAGS", "TARGET_CC", "SDKROOT",
        )
        for name in overrides:
            with self.subTest(name=name):
                env = self.environment | {name: "do-not-disclose-this-value"}
                with self.assertRaises(ValueError) as result:
                    capture.check_build_inputs(self.root, env)
                self.assertIn(name, str(result.exception))
                self.assertNotIn("do-not-disclose-this-value", str(result.exception))

    def test_workspace_ancestor_and_default_home_configs_fail_without_reading(self):
        for directory in (self.root / ".cargo", self.root.parent / ".cargo",
                          self.home / ".cargo"):
            directory.mkdir(exist_ok=True)
            for filename in ("config", "config.toml"):
                with self.subTest(directory=directory, filename=filename):
                    path = directory / filename
                    path.write_text('token = "private-config-value"\n')
                    with patch.object(Path, "read_text", side_effect=AssertionError("config read")):
                        with self.assertRaisesRegex(ValueError, "Cargo configuration") as result:
                            capture.check_build_inputs(self.root, self.environment)
                    self.assertNotIn("private-config-value", str(result.exception))
                    path.unlink()

    def test_explicit_and_relative_cargo_home_configs_are_checked(self):
        for cargo_home in (str(self.directory / "cargo-home"), "relative-cargo-home"):
            directory = Path(cargo_home)
            if not directory.is_absolute():
                directory = self.root / directory
            directory.mkdir()
            for filename in ("config", "config.toml"):
                with self.subTest(cargo_home=cargo_home, filename=filename):
                    path = directory / filename
                    path.write_text("[build]\ntarget = 'x86_64-apple-darwin'\n")
                    with self.assertRaisesRegex(ValueError, "Cargo configuration"):
                        capture.check_build_inputs(self.root, self.environment | {"CARGO_HOME": cargo_home})
                    path.unlink()

    def test_committed_manifest_profile_overrides_are_recorded(self):
        (self.root / "Cargo.toml").write_text("[profile.bench]\nlto = true\ncodegen-units = 1\n")
        self.assertEqual(capture.check_build_inputs(self.root, self.environment)["workspace_profiles"],
                         {"bench": {"lto": True, "codegen-units": 1}})

    def test_configuration_appearing_after_admission_is_rejected(self):
        capture.check_build_inputs(self.root, self.environment)
        directory = self.root / ".cargo"
        directory.mkdir()
        (directory / "config.toml").write_text("[profile.bench]\nopt-level = 0\n")
        with self.assertRaisesRegex(ValueError, "Cargo configuration"):
            capture.check_build_inputs(self.root, self.environment)

    def test_cli_rejects_override_before_source_or_toolchain_access(self):
        env = {name: value for name, value in os.environ.items()
               if not name.startswith(("CARGO_", "RUSTC", "RUSTDOC"))
               and name not in {"RUST_TARGET_PATH", "RUSTUP_TOOLCHAIN"}}
        env.update(self.environment, CARGO_BUILD_TARGET="private-override-value")
        destination = self.directory / "results"
        result = subprocess.run([sys.executable, str(SCRIPT), str(destination)],
                                env=env, text=True, capture_output=True, timeout=10)
        self.assertEqual(result.returncode, 2)
        self.assertIn("CARGO_BUILD_TARGET", result.stderr)
        self.assertNotIn("private-override-value", result.stdout + result.stderr)
        self.assertFalse(destination.exists())

    def test_toolchain_receipt_queries_the_resolved_compiler(self):
        paths = {}
        for name in ("cargo", "rustc", "rustdoc"):
            path = self.directory / name
            path.write_text(name)
            paths[name] = str(path)

        def command_output(*command):
            if command[:4] == ("rustup", "which", "--toolchain", "1.99.0"):
                return paths[command[4]]
            if command == (paths["rustc"], "-Vv"):
                return "rustc 1.99.0\nhost: aarch64-apple-darwin"
            if command == (paths["cargo"], "--version"):
                return "cargo 1.99.0"
            self.fail(f"unexpected executable query: {command}")

        with patch.object(capture, "output", side_effect=command_output):
            record = capture.toolchain()
        self.assertEqual(record["host_target"], "aarch64-apple-darwin")
        self.assertEqual(record["executables"]["rustc"]["path"], paths["rustc"])
        self.assertEqual(len(record["executables"]["rustc"]["sha256"]), 64)

    def test_completed_receipt_controls_inputs_and_rejects_late_config(self):
        # Only Cargo's external output is simulated; admission, environment
        # construction, final recheck and receipt writing execute normally.
        identities = [f"core_baseline/{query}/{state}/{size}"
                      for query in ("marker", "reference", "nested_reference")
                      for state in ("cold", "ast_warm_result_cold", "result_warm")
                      for size in (972, 10125)]
        identities += [f"core_baseline/mutation_then_nested_query/{size}" for size in (972, 10125)]
        identities += [f"history_baseline/{query}/{size}"
                       for query in ("direct_full", "direct_first_day", "http_first_day")
                       for size in (1000, 10000)]
        (self.root / "Cargo.lock").write_text('[[package]]\nname = "criterion"\nversion = "0.8.2"\n')
        selected = {"executables": {name: {"path": f"/selected/{name}"}
                                     for name in ("cargo", "rustc", "rustdoc")}}
        self.environment.update(RUSTFLAGS="caller flags", CARGO_BUILD_JOBS="99",
                                CARGO_INCREMENTAL="1", CARGO_TARGET_DIR="caller-target")
        for late_config in (False, True):
            with self.subTest(late_config=late_config):
                destination = self.directory / f"output-{late_config}"

                def cargo_output(command, **kwargs):
                    self.assertEqual(command[0], "/selected/cargo")
                    env = kwargs["env"]
                    self.assertEqual(env["RUSTC"], "/selected/rustc")
                    self.assertEqual(env["RUSTDOC"], "/selected/rustdoc")
                    self.assertEqual(env["RUSTFLAGS"], "-Dwarnings")
                    self.assertEqual(env["CARGO_BUILD_JOBS"], "2")
                    self.assertEqual(env["CARGO_INCREMENTAL"], "0")
                    self.assertEqual(env["CARGO_TARGET_DIR"], str(self.root / "target"))
                    if "rusty-haystack-server" in command:
                        for index, identity in enumerate(identities):
                            directory = Path(env["CRITERION_HOME"]) / str(index) / "m0"
                            directory.mkdir(parents=True)
                            for filename, value in (("benchmark", {"full_id": identity}),
                                                    ("estimates", {}), ("sample", {})):
                                (directory / f"{filename}.json").write_text(json.dumps(value))
                        if late_config:
                            directory = self.root / ".cargo"
                            directory.mkdir()
                            (directory / "config.toml").write_text('token = "private-config-value"\n')
                    return subprocess.CompletedProcess(command, 0)

                with (patch.object(capture, "ROOT", self.root),
                      patch.object(capture, "source", return_value={"commit": "clean-fixture", "dirty": False}),
                      patch.object(capture, "toolchain", return_value=selected),
                      patch.object(capture, "output", return_value="1"),
                      patch.object(capture.platform, "platform", return_value="test host"),
                      patch.object(capture.subprocess, "run", side_effect=cargo_output),
                      patch.dict(os.environ, self.environment, clear=True),
                      patch.object(sys, "argv", [str(SCRIPT), str(destination)]),
                      contextlib.redirect_stdout(io.StringIO())):
                    if late_config:
                        with self.assertRaisesRegex(ValueError, "Cargo configuration"):
                            capture.main()
                    else:
                        capture.main()
                    self.assertEqual(dict(os.environ), self.environment)
                text = (destination / "baseline.json").read_text()
                record = json.loads(text)
                self.assertEqual(record["status"], "failed" if late_config else "complete")
                self.assertNotIn("private-config-value", text)
                self.assertEqual(record["build_inputs"]["cargo_config_files"], [])


if __name__ == "__main__":
    unittest.main()
