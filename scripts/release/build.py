#!/usr/bin/env python3
"""Construct one candidate once; all output stays in fresh, task-owned directories."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tomllib
import zipfile

import artifacts as a


def run(command, work, *, cwd, env=None, label):
    with (work / f"{label}.log").open("x") as log:
        result = subprocess.run(command, cwd=cwd, env=env, stdout=log, stderr=subprocess.STDOUT, timeout=1800)
    a.require(result.returncode == 0, f"{label} failed; retained log: {work / (label + '.log')}")


def manifest_in(root):
    project = tomllib.loads((root / "pyproject.toml").read_text())
    relative = project.get("tool", {}).get("maturin", {}).get("manifest-path", "Cargo.toml")
    a.safe_name(relative)
    manifest = root / relative
    a.require(manifest.is_file(), "source archive manifest is missing")
    return manifest


def repair_sdist(raw, output, work, source_lock, base_env):
    extracted = work / "raw-extracted"
    a.extract(raw, extracted)
    roots = list(extracted.iterdir())
    a.require(len(roots) == 1 and roots[0].is_dir(), "invalid source archive root")
    root = roots[0]
    locks = list(root.rglob("Cargo.lock"))
    a.require(len(locks) == 1, "source archive lock inventory mismatch")
    lock = locks[0]
    a.require(a.digest(lock) == a.digest(source_lock), "raw source archive did not copy the checkout lock")
    before = lock.read_text()
    metadata_env = dict(base_env, CARGO_TARGET_DIR=str(work / "source-metadata-target"))
    tools = a.tool_info(repo=root, target="source", env=metadata_env)
    env = a.build_environment(tools, metadata_env)
    command = [tools["executables"]["cargo"]["path"], "metadata", "--offline", "--format-version", "1", "--manifest-path", str(manifest_in(root))]
    run(command, work, cwd=root, env=env, label="source-lock-prune")
    removed = a.lock_prune(before, lock.read_text())
    run(command + ["--locked"], work, cwd=root, env=env, label="source-locked-metadata")
    a.require(a.tool_info(repo=root, target="source", env=metadata_env) == tools, "source metadata compiler context changed")
    with tarfile.open(output, "w:gz") as archive:
        archive.add(root, arcname=root.name)
    return removed, tools


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("kind", choices=("cli", "wheel", "sdist"))
    parser.add_argument("--repo", type=Path, required=True)
    parser.add_argument("--source", required=True)
    parser.add_argument("--target", required=True, help="Rust target triple, or source for the sdist")
    parser.add_argument("--python", default=sys.executable, help="explicit trusted interpreter; Python artifacts require pinned Maturin")
    parser.add_argument("--bundle", type=Path, required=True)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--cross-image", help="only for the existing Linux aarch64 cross tuple")
    args = parser.parse_args()
    try:
        repo, bundle, work = args.repo.resolve(), args.bundle.resolve(), args.work.resolve()
        source = a.source_info(repo, args.source)
        version = a.project(repo)
        a.outside_checkout(bundle, repo); a.outside_checkout(work, repo)
        work.mkdir(parents=True, exist_ok=False)
        (bundle / "files").mkdir(parents=True, exist_ok=False)
        base_env = dict(os.environ)
        tools = a.tool_info(args.python if args.kind != "cli" else None, repo=repo, target=args.target, env=base_env)
        env = a.build_environment(tools, base_env)
        if args.kind == "cli":
            a.require(args.target in a.BINARY_TARGETS, "unsupported CLI target")
            command = [tools["executables"]["cargo"]["path"]]
            if args.cross_image:
                a.require(args.target == "aarch64-unknown-linux-gnu", "unexpected cross target")
                tools["cross"] = a.output(["cross", "--version"])
                a.require(tools["cross"].startswith("cross 0.2.5"), "unexpected cross version")
                image = json.loads(a.output(["docker", "image", "inspect", args.cross_image]))[0]
                a.require(image["RepoDigests"], "cross image has no immutable registry digest")
                tools["container"] = {"id": image["Id"], "repo_digests": image["RepoDigests"]}
                env["CROSS_TARGET_AARCH64_UNKNOWN_LINUX_GNU_IMAGE"] = image["RepoDigests"][0]
                # cross mounts the selected host toolchain into its container.
                # Host absolute compiler paths cannot be passed into that mount.
                env.pop("RUSTC"); env.pop("RUSTDOC")
                command = ["cross", "+" + a.RUST]
            run(command + ["build", "--locked", "--release", "--target", args.target, "-p", "rusty-haystack-cli"], work, cwd=repo, env=env, label="cli-build")
            binary = "haystack.exe" if "windows" in args.target else "haystack"
            target_dir = Path(tools["context"]["target_dir"])
            executable = target_dir / args.target / "release" / binary
            suffix = "zip" if "windows" in args.target else "tar.gz"
            path = bundle / "files" / f"haystack-{version}-{args.target}.{suffix}"
            if suffix == "zip":
                with zipfile.ZipFile(path, "x", compression=zipfile.ZIP_DEFLATED) as archive:
                    archive.write(executable, binary); archive.write(repo / "LICENSE", "LICENSE")
            else:
                with tarfile.open(path, "w:gz") as archive:
                    archive.add(executable, arcname=binary); archive.add(repo / "LICENSE", arcname="LICENSE")
            receipt = a.seal(repo, bundle, source, args.target, "cli", tools)
        elif args.kind == "wheel":
            run([args.python, "-m", "maturin", "build", "--locked", "--release", "--target", args.target, "--interpreter", args.python, "--out", str(bundle / "files"), "-m", str(repo / "rusty-haystack/Cargo.toml")], work, cwd=repo, env=env, label="wheel-build")
            version_parts = tools["python"]["version"].split(".")
            tag = "cp" + "".join(version_parts[:2])
            receipt = a.seal(repo, bundle, source, args.target, "wheel", tools, python_tag=tag)
        else:
            a.require(args.target == "source", "sdist target must be source")
            raw_dir = work / "raw-sdist"; raw_dir.mkdir()
            run([args.python, "-m", "maturin", "sdist", "--out", str(raw_dir), "-m", str(repo / "rusty-haystack/Cargo.toml")], work, cwd=repo, env=env, label="raw-sdist-build")
            raw_files = list(raw_dir.iterdir())
            a.require(len(raw_files) == 1, "unexpected raw sdist inventory")
            raw = raw_files[0]
            removed, metadata_tools = repair_sdist(raw, bundle / "files" / raw.name, work, repo / "Cargo.lock", base_env)
            tools["source_metadata"] = metadata_tools
            receipt = a.seal(repo, bundle, source, "source", "sdist", tools, removed=removed)
        after = a.tool_info(args.python if args.kind != "cli" else None, repo=repo, target=args.target, env=base_env)
        a.require(all(tools[key] == value for key, value in after.items()), "builder compiler context changed")
        a.require(a.source_info(repo, args.source) == source, "source changed during artifact construction")
        print(json.dumps({"status": "built", "artifact": receipt["artifact"]}, sort_keys=True))
    except (a.Invalid, OSError, ValueError, KeyError, subprocess.SubprocessError, tarfile.TarError, zipfile.BadZipFile) as error:
        print(json.dumps({"status": "failed", "error": str(error)[:512]}), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
