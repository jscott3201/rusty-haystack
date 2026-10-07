#!/usr/bin/env python3
"""Qualify downloaded candidates; never rebuild or replace the release wheel."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import tomllib

import artifacts as a
from build import manifest_in


def run(command, work, label, *, cwd, env=None, input=None):
    result = subprocess.run(command, cwd=cwd, env=env, input=input, text=True, capture_output=True, timeout=1800)
    (work / (label + ".stdout.log")).write_text(result.stdout)
    (work / (label + ".stderr.log")).write_text(result.stderr)
    a.require(result.returncode == 0, f"{label} failed; logs retained in {work}")
    return result.stdout.strip()


def clean_environment(python=None):
    env = dict(os.environ)
    for name in ("PYTHONPATH", "PYTHONHOME", "VIRTUAL_ENV", "PYO3_PYTHON", "PYO3_ENVIRONMENT_SIGNATURE"):
        env.pop(name, None)
    env["CARGO_BUILD_JOBS"] = "2"
    env["CARGO_NET_OFFLINE"] = "true"
    env["PIP_DISABLE_PIP_VERSION_CHECK"] = "1"
    if python:
        env["PYO3_PYTHON"] = str(python)
        env["VIRTUAL_ENV"] = str(python.parent.parent)
        env["PATH"] = str(python.parent) + os.pathsep + env["PATH"]
    return env


def installed_smoke(path, kind, repo, work, version, interpreter):
    venv = work / "venv"
    scratch = work / "consumer-cwd"; scratch.mkdir()
    run([interpreter, "-m", "venv", str(venv)], work, "create-venv", cwd=scratch, env=clean_environment())
    python = venv / ("Scripts/python.exe" if os.name == "nt" else "bin/python")
    env = clean_environment(python)
    checks = {"installed-smoke", "typing-layout"}
    source_lock_before = None
    source_tools = None
    if kind == "sdist":
        source_dir = work / "source"
        a.extract(path, source_dir)
        roots = list(source_dir.iterdir()); a.require(len(roots) == 1, "invalid extracted source inventory")
        root = roots[0]
        pyproject = tomllib.loads((root / "pyproject.toml").read_text())
        a.require(pyproject["build-system"] == {"requires": [f"maturin=={a.MATURIN}"], "build-backend": "maturin"}, "sdist backend pin mismatch")
        a.require(pyproject["tool"]["maturin"]["locked"] is True, "sdist PEP517 builds are not locked")
        locks = list(root.rglob("Cargo.lock")); a.require(len(locks) == 1, "source lock inventory mismatch")
        source_lock_before = a.digest(locks[0])
        run([str(python), "-m", "pip", "install", "--disable-pip-version-check", f"maturin=={a.MATURIN}"], work, "install-pinned-backend", cwd=scratch, env=env)
        a.require(run([str(python), "-m", "maturin", "--version"], work, "backend-version", cwd=scratch, env=env) == f"maturin {a.MATURIN}", "installed backend version mismatch")
        source_base_env = dict(env)
        source_tools = a.tool_info(str(python), repo=root, target="source", env=source_base_env)
        env = a.build_environment(source_tools, source_base_env)
        run([source_tools["executables"]["cargo"]["path"], "metadata", "--offline", "--locked", "--format-version", "1", "--manifest-path", str(manifest_in(root))], work, "locked-source-metadata", cwd=root, env=env)
        install = root
        checks |= {"locked-source-metadata", "pep517-install"}
    else:
        install = path
    run([str(python), "-m", "pip", "install", "--no-index", "--no-deps", "--no-cache-dir", "--no-build-isolation", str(install)], work, "install-artifact", cwd=scratch, env=env)
    if source_lock_before:
        a.require(a.digest(locks[0]) == source_lock_before, "PEP517 build mutated the sealed source lock")
        a.require(a.tool_info(str(python), repo=root, target="source", env=source_base_env) == source_tools, "PEP517 compiler context changed")
    observed = json.loads(run([str(python), "-I", str(repo / "scripts/release/consumer.py"), "--prefix", str(venv), "--version", version], work, "installed-smoke", cwd=scratch, env=env))
    if source_tools:
        observed["source_build_tools"] = source_tools
    return checks, observed


def cli_smoke(path, work, version):
    extracted = work / "cli"; a.extract(path, extracted)
    binary = extracted / ("haystack.exe" if os.name == "nt" else "haystack")
    binary.chmod(0o755)
    scratch = work / "consumer-cwd"; scratch.mkdir()
    actual = run([str(binary), "--version"], work, "cli-version", cwd=scratch)
    a.require(actual == "haystack " + version, "CLI --version mismatch")
    fixture = 'ver:"3.0"\nid,dis,site,point\n@artifact-site,"Archive Site",M,N\n@artifact-point,"Archive Point",N,M\n'
    result = run([str(binary), "export", "--format", "json3", "--filter", "site"], work, "cli-fixture", cwd=scratch, input=fixture)
    grid = json.loads(result)
    a.require(len(grid["rows"]) == 1, "CLI fixture row count mismatch")
    row = grid["rows"][0]
    a.require(row["id"] == "r:artifact-site" and row["dis"] == "s:Archive Site" and row["site"] == "m:" and row.get("point") is None, "CLI fixture output mismatch")
    return {"cli-version", "cli-fixture"}, {"version_output": actual, "fixture_rows": grid["rows"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, required=True); parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--source", required=True); parser.add_argument("--profile", choices=("native", "binaries", "python"), required=True)
    parser.add_argument("--target"); parser.add_argument("--python", default=sys.executable)
    parser.add_argument("--work", type=Path, required=True); parser.add_argument("--evidence", type=Path, required=True)
    args = parser.parse_args()
    try:
        repo, incoming, work, evidence = (path.resolve() for path in (args.repo, args.input, args.work, args.evidence))
        for path in (incoming, work, evidence): a.outside_checkout(path, repo)
        version = a.project(repo)
        found = a.inventory(incoming, profile=args.profile, target=args.target, source=args.source, version=version, source_lock=a.digest(repo / "Cargo.lock"), source_lock_text=(repo / "Cargo.lock").read_text())
        work.mkdir(parents=True, exist_ok=False); evidence.mkdir(parents=True, exist_ok=False)
        runtime_target = args.target if args.profile == "native" else "x86_64-unknown-linux-gnu"
        selected_tools = a.tool_info(repo=repo, target=runtime_target, env=clean_environment())
        host = next(line[6:] for line in selected_tools["rustc"].splitlines() if line.startswith("host: "))
        a.require(host == runtime_target, "qualification requires the selected native host")
        for key, (path, receipt_path, receipt) in sorted(found.items()):
            kind, target, py = key
            artifact_work = work / path.name; artifact_work.mkdir()
            checks = {"provenance", "archive-metadata"}; observed = {}
            if kind == "cli" and target == runtime_target:
                more, observed = cli_smoke(path, artifact_work, version); checks |= more
            elif kind == "sdist" or (kind == "wheel" and target == runtime_target and py == "cp312"):
                more, observed = installed_smoke(path, kind, repo, artifact_work, version, args.python); checks |= more
            a.require(a.digest(path) == receipt["artifact"]["sha256"], "qualification changed original candidate bytes")
            a.write_json(evidence / (path.name + ".json"), {"schema": 1, "status": "passed", "artifact": receipt["artifact"], "receipt_sha256": a.digest(receipt_path), "checks": sorted(checks), "environment": {"host": host, "os": platform.platform(), "observed": observed}, "sdist_build_is_validation_only": kind == "sdist"})
        print(json.dumps({"status": "qualified", "count": len(found), "runtime_target": runtime_target}))
    except (a.Invalid, OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        print(json.dumps({"status": "failed", "error": str(error)[:512]}), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
