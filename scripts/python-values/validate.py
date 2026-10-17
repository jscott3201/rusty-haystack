#!/usr/bin/env python3
"""Validate a fresh installed wheel and real positive/negative static consumers.

This is local selected-host evidence, not release provenance or a platform matrix.
The caller builds the wheel; no source or stub paths enter the consumer import path.
"""
from __future__ import annotations

import argparse
import ast
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tomllib

CHECKER = "pyright 1.1.413"
REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "scripts/release"))
import artifacts
sys.path.pop(0)


def run(command, work, label, *, cwd, expected=0):
    env = dict(os.environ)
    for name in ("PYTHONPATH", "PYTHONHOME", "VIRTUAL_ENV"):
        env.pop(name, None)
    process = subprocess.Popen(command, cwd=cwd, env=env, text=True, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, start_new_session=os.name != "nt")
    try:
        stdout, stderr = process.communicate(timeout=300)
    except subprocess.TimeoutExpired:
        if os.name != "nt":
            os.killpg(process.pid, signal.SIGKILL)
        else:
            process.kill()
        stdout, stderr = process.communicate()
        (work / (label + ".stdout")).write_text(stdout)
        (work / (label + ".stderr")).write_text(stderr)
        raise RuntimeError(f"{label} exceeded 300 seconds; process group stopped; output retained in {work}")
    (work / (label + ".stdout")).write_text(stdout)
    (work / (label + ".stderr")).write_text(stderr)
    if process.returncode != expected:
        raise RuntimeError(f"{label} exited {process.returncode}, expected {expected}; retained output in {work}")
    return stdout


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--wheel", type=Path, required=True)
    parser.add_argument("--python", type=Path, required=True)
    parser.add_argument("--pyright", type=Path, required=True)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--watch-fixture", type=Path, required=True)
    parser.add_argument("--abi", choices=("cp312", "cp314t"), default="cp312")
    parser.add_argument("--fixture-receipt", type=Path)
    args = parser.parse_args()
    work = args.work.resolve()
    if work.is_relative_to(REPO):
        raise ValueError("consumer work must be outside the checkout")
    work.mkdir(parents=True, exist_ok=False)
    scratch = work / "consumer"; scratch.mkdir()
    checker = str(args.pyright.resolve())
    if run([checker, "--version"], work, "checker-version", cwd=scratch).strip() != CHECKER:
        raise ValueError(f"expected {CHECKER}")
    venv = work / "venv"
    run([str(args.python.resolve()), "-I", "-m", "venv", str(venv)], work, "create-venv", cwd=scratch)
    python = venv / ("Scripts/python.exe" if os.name == "nt" else "bin/python")
    wheel = args.wheel.resolve()
    run([str(python), "-I", "-m", "pip", "install", "--no-index", "--no-deps", "--no-cache-dir", str(wheel)], work, "install-wheel", cwd=scratch)
    manifest = work / "expected-typing.json"
    manifest.write_text(json.dumps(artifacts.typing_sources(REPO), sort_keys=True))
    wheel_manifest = work / "expected-wheel-members.json"
    wheel_members = {name: hashlib.sha256(data).hexdigest() for name, (data, _) in artifacts.members(wheel).items() if name.startswith("rusty_haystack/")}
    wheel_manifest.write_text(json.dumps(wheel_members, sort_keys=True))
    version = tomllib.loads((REPO / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    layout_command = [str(python), "-I", str(REPO / "scripts/release/consumer.py"), "--prefix", str(venv), "--version", version, "--typing-manifest", str(manifest), "--wheel-manifest", str(wheel_manifest), "--abi", args.abi]
    installed = json.loads(run(layout_command, work, "installed-layout", cwd=scratch))
    for name in ("positive.py", "negative.py", "async-runtime.py", "application-runtime.py", "watch-runtime.py", "h5-runtime.py", "sync-runtime.py", "concurrency-runtime.py", "runtime-runner.py"):
        shutil.copyfile(Path(__file__).with_name(name), scratch / name)
    shutil.copyfile(REPO / "scripts/release/runtime_profile.py", scratch / "runtime_profile.py")
    shutil.copyfile(REPO / "rusty-haystack/tests/async_peer.py", scratch / "async_peer.py")
    shutil.copyfile(REPO / "rusty-haystack/tests/sync_peer.py", scratch / "sync_peer.py")
    shutil.copyfile(REPO / "rusty-haystack/tests/application_fixture.py", scratch / "application_fixture.py")
    shutil.copyfile(REPO / "rusty-haystack/tests/watch_fixture.py", scratch / "watch_fixture.py")
    watch_fixture = scratch / args.watch_fixture.name
    shutil.copy2(args.watch_fixture.resolve(), watch_fixture)
    shutil.copyfile(REPO / "haystack-core/tests/fixtures/typed-v1-entity.json", scratch / "typed-v1-entity.json")
    (scratch / "pyrightconfig.json").write_text(json.dumps({"typeCheckingMode": "strict", "pythonVersion": "3.11", "extraPaths": []}))
    runtime_modes = {}
    def runtime(name, *arguments):
        label = "positive-runtime" if name == "positive" else name
        observation = work / (label + ".mode.json")
        value = run([str(python), "-I", str(scratch / "runtime-runner.py"), "--abi", args.abi,
                     "--prefix", str(venv), "--evidence", str(observation), str(scratch / (name + ".py")), *arguments], work, label, cwd=scratch)
        runtime_modes[label] = json.loads(observation.read_text())
        return value
    runtime("positive")
    async_runtime = json.loads(runtime("async-runtime"))
    application_runtime = json.loads(runtime("application-runtime"))
    h5_runtime = json.loads(runtime("h5-runtime"))
    sync_runtime = json.loads(runtime("sync-runtime"))
    concurrency_runtime = json.loads(runtime("concurrency-runtime"))
    expected_helper = REPO / "rusty-haystack/rusty_haystack/_application.py"
    if application_runtime["entry_helper_sha256"] != hashlib.sha256(expected_helper.read_bytes()).hexdigest():
        raise AssertionError("installed application entry helper differs from source")
    watch_runtime = json.loads(runtime("watch-runtime", "--fixture", str(watch_fixture)))
    expected_watch_helper = REPO / "rusty-haystack/rusty_haystack/_watch.py"
    if watch_runtime["entry_helper_sha256"] != artifacts.digest(expected_watch_helper):
        raise AssertionError("installed watch entry helper differs from source")
    if watch_runtime["fixture_sha256"] != artifacts.digest(args.watch_fixture.resolve()):
        raise AssertionError("copied first-party watch executable differs from input")
    watch_inputs = {str(path.relative_to(REPO)): artifacts.digest(path) for path in (
        expected_watch_helper, REPO / "rusty-haystack/tests/watch_fixture.py",
        REPO / "scripts/python-values/watch-runtime.py", REPO / "haystack-server/examples/python_watch_fixture.rs",
    )}
    command = [checker, "--pythonpath", str(python), "--outputjson"]
    positive = json.loads(run(command + ["positive.py"], work, "positive-static", cwd=scratch))
    # PyO3 registers these modules in sys.modules; no Python source file exists
    # for them. Keep the checker's source-location warnings in the receipt while
    # requiring the corresponding installed runtime imports above to succeed.
    native_modules = {"auth", "client", "codecs", "data", "filter", "graph", "kinds", "ontology", "server", "units"}
    source_warnings = positive["generalDiagnostics"]
    imported_lines = {node.module.removeprefix("rusty_haystack."): node.lineno - 1 for node in ast.parse((scratch / "positive.py").read_text()).body if isinstance(node, ast.ImportFrom) and node.module and node.module.startswith("rusty_haystack.")}
    expected_warnings = {(f'Import "rusty_haystack.{name}" could not be resolved from source', imported_lines[name]) for name in native_modules}
    actual_warnings = {(item["message"], item["range"]["start"]["line"]) for item in source_warnings}
    if positive["summary"]["errorCount"] != 0 or actual_warnings != expected_warnings or any(item["severity"] != "warning" or item.get("rule") != "reportMissingModuleSource" or Path(item["file"]).resolve() != scratch / "positive.py" for item in source_warnings):
        raise AssertionError("positive static consumer has diagnostics")
    negative = json.loads(run(command + ["negative.py"], work, "negative-static", cwd=scratch, expected=1))
    expected_lines = {i for i, line in enumerate((scratch / "negative.py").read_text().splitlines()) if "# reject" in line}
    diagnostics = negative["generalDiagnostics"]
    actual_lines = {item["range"]["start"]["line"] for item in diagnostics if item["severity"] == "error" and Path(item["file"]).resolve() == scratch / "negative.py"}
    if actual_lines != expected_lines or any(item["severity"] != "error" or Path(item["file"]).resolve() != scratch / "negative.py" for item in diagnostics):
        raise AssertionError("negative static diagnostics did not match the marked consumer lines")
    result = {"wheel": str(wheel), "wheel_sha256": artifacts.digest(wheel), "checker": CHECKER, "static_python_floor": "3.11", "positive_errors": 0, "native_submodule_source_warnings": source_warnings, "negative_lines_rejected": len(expected_lines), "installed": installed, "async_runtime": async_runtime, "application_runtime": application_runtime, "watch_runtime": watch_runtime, "watch_inputs": watch_inputs}
    result["h5_runtime"] = h5_runtime
    result["sync_runtime"] = sync_runtime
    result["abi"] = args.abi
    result["runtime_modes"] = runtime_modes
    result["concurrency_runtime"] = concurrency_runtime
    result["installed_after"] = json.loads(run(layout_command, work, "installed-layout-after", cwd=scratch))
    result["consumer_inputs"] = artifacts.python_consumer_inputs(REPO)
    result["watch_fixture_build"] = artifacts.read_json(args.fixture_receipt) if args.fixture_receipt else {"status": "unbound", "sha256": artifacts.digest(args.watch_fixture)}
    (work / "result.json").write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    print(json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    main()
