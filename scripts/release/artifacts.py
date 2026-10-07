#!/usr/bin/env python3
"""Seal, verify and stage release files. No publication or network operations."""
from __future__ import annotations

import argparse
import email.parser
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import struct
import subprocess
import sys
import tarfile
import tomllib
import zipfile

RUST = "1.99.0"
MATURIN = "1.15.0"
BINARY_TARGETS = ("x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", "x86_64-apple-darwin", "aarch64-apple-darwin", "x86_64-pc-windows-msvc")
WHEEL_TARGETS = BINARY_TARGETS[:4]
PYTHONS = ("cp311", "cp312", "cp313")
MAX_BYTES = 512 * 1024 * 1024


class Invalid(ValueError):
    pass


def require(condition, message):
    if not condition:
        raise Invalid(message)


def pairs(values):
    result = {}
    for key, value in values:
        require(key not in result, "duplicate JSON key")
        result[key] = value
    return result


def read_json(path):
    require(path.stat().st_size <= 1024 * 1024, "receipt too large")
    return json.loads(path.read_text(), object_pairs_hook=pairs)


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("x", encoding="utf-8") as stream:
        json.dump(value, stream, indent=2, sort_keys=True)
        stream.write("\n")


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def output(command, *, cwd=None, env=None):
    result = subprocess.run(command, cwd=cwd, env=env, text=True, capture_output=True, timeout=120)
    require(result.returncode == 0, f"command failed: {Path(command[0]).name} (exit {result.returncode})")
    return result.stdout.strip()


def project(repo):
    cargo = tomllib.loads((repo / "Cargo.toml").read_text())
    pyproject = tomllib.loads((repo / "rusty-haystack/pyproject.toml").read_text())
    version = cargo["workspace"]["package"]["version"]
    require(pyproject["project"]["name"] == "rusty-haystack", "unexpected Python package name")
    require(pyproject["project"].get("dynamic") == ["version"], "Python version must come from Cargo")
    for path in ("rusty-haystack", "haystack-cli"):
        package = tomllib.loads((repo / path / "Cargo.toml").read_text())["package"]
        require(package["version"] == {"workspace": True}, "package does not inherit workspace version")
    return version


def source_info(repo, expected, allow_dirty=False):
    revision = output(["git", "rev-parse", "HEAD"], cwd=repo)
    require(re.fullmatch(r"[0-9a-f]{40}", expected) and revision == expected, "source revision mismatch")
    dirty = bool(output(["git", "status", "--porcelain", "--untracked-files=normal"], cwd=repo))
    require(allow_dirty or not dirty, "candidate construction requires a clean source revision")
    return {"revision": revision, "dirty": dirty, "lock_sha256": digest(repo / "Cargo.lock")}


def tool_info(python=None, maturin=None):
    rustc = output(["rustc", "-vV"])
    release = next((line[9:] for line in rustc.splitlines() if line.startswith("release: ")), None)
    require(release == RUST, "unexpected Rust compiler version")
    info = {"rustc": rustc, "cargo": output(["cargo", "--version"])}
    if python:
        info["python"] = json.loads(output([python, "-I", "-c", "import json,platform,sys,sysconfig; print(json.dumps({'implementation':platform.python_implementation(),'version':platform.python_version(),'cache_tag':sys.implementation.cache_tag,'platform':sysconfig.get_platform()}))"]))
        info["maturin"] = output([maturin, "--version"] if maturin else [python, "-m", "maturin", "--version"])
        require(info["maturin"] == f"maturin {MATURIN}", "unexpected Maturin backend")
    return info


def safe_name(name):
    require(isinstance(name, str) and name and "\\" not in name and ":" not in name and "\x00" not in name, "unsafe archive path")
    stripped = name.rstrip("/")
    path = PurePosixPath(stripped)
    require(not path.is_absolute() and all(part not in ("", ".", "..") for part in stripped.split("/")), "unsafe archive path")
    return stripped


def members(path):
    """Read only bounded regular files/directories; never follow archive links."""
    entries = {}
    total = 0
    with (zipfile.ZipFile(path) if path.suffix in (".zip", ".whl") else tarfile.open(path, "r:gz")) as archive:
        items = archive.infolist() if isinstance(archive, zipfile.ZipFile) else archive.getmembers()
        require(len(items) <= 50_000, "too many archive entries")
        seen = set()
        for item in items:
            is_zip = isinstance(archive, zipfile.ZipFile)
            name = safe_name(item.filename if is_zip else item.name)
            require(name.casefold() not in seen, "duplicate archive path")
            seen.add(name.casefold())
            directory = item.is_dir() if is_zip else item.isdir()
            mode = (item.external_attr >> 16) if is_zip else item.mode
            require((is_zip and not stat.S_ISLNK(mode)) or (not is_zip and (item.isfile() or directory)), "archive link or special file")
            size = item.file_size if is_zip else item.size
            total += size
            require(0 <= size <= MAX_BYTES and total <= MAX_BYTES, "archive exceeds size limit")
            if directory:
                continue
            data = archive.read(item) if is_zip else archive.extractfile(item).read()
            require(len(data) == size, "truncated archive member")
            entries[name] = (data, mode & 0o777)
    for name in entries:
        require(not any(str(parent) in entries for parent in PurePosixPath(name).parents), "archive file shadows a directory")
    return entries


def extract(path, destination):
    entries = members(path)  # Validate the complete inventory before writing.
    destination.mkdir(parents=True, exist_ok=False)
    for name, (data, mode) in entries.items():
        target = destination / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
        target.chmod(mode or 0o644)
    return entries


def canonical(name):
    return re.sub(r"[-_.]+", "-", name).lower()


def metadata(text):
    parsed = email.parser.Parser().parsestr(text)
    require(len(parsed.get_all("Name", [])) == len(parsed.get_all("Version", [])) == 1, "missing or duplicate package metadata")
    return parsed


def native_identity(data):
    if data[:4] == b"\x7fELF":
        require(len(data) >= 20 and data[5] in (1, 2), "invalid ELF header")
        machine = int.from_bytes(data[18:20], "little" if data[5] == 1 else "big")
        return "linux", {62: "x86_64", 183: "aarch64"}.get(machine)
    if data[:4] in (b"\xcf\xfa\xed\xfe", b"\xfe\xed\xfa\xcf"):
        machine = int.from_bytes(data[4:8], "little" if data[0] == 0xcf else "big")
        return "darwin", {0x1000007: "x86_64", 0x100000c: "aarch64"}.get(machine)
    if data[:2] == b"MZ" and len(data) >= 64:
        offset = struct.unpack_from("<I", data, 60)[0]
        require(data[offset:offset + 4] == b"PE\0\0", "invalid PE header")
        machine = int.from_bytes(data[offset + 4:offset + 6], "little")
        return "windows", {0x8664: "x86_64", 0xaa64: "aarch64"}.get(machine)
    raise Invalid("unrecognized native executable format")


def target_identity(target):
    require(target in BINARY_TARGETS, "unsupported target")
    return ("linux" if "linux" in target else "darwin" if "darwin" in target else "windows", target.split("-")[0])


def inspect_file(path, kind, version, target, python_tag=None):
    entries = members(path)
    if kind == "cli":
        binary = "haystack.exe" if "windows" in target else "haystack"
        require(set(entries) == {binary, "LICENSE"}, "unexpected CLI archive inventory")
        require(native_identity(entries[binary][0]) == target_identity(target), "CLI target does not match native header")
        expected = f"haystack-{version}-{target}." + ("zip" if "windows" in target else "tar.gz")
        require(path.name == expected, "CLI filename mismatch")
        return {"name": "rusty-haystack-cli", "version": version}, None
    if kind == "wheel":
        require(target in WHEEL_TARGETS and python_tag in PYTHONS, "unsupported wheel tuple")
        fields = path.name.removesuffix(".whl").split("-")
        require(path.suffix == ".whl" and len(fields) == 5, "unexpected wheel filename")
        dist, wheel_version, interpreter, abi, platforms = fields
        require(canonical(dist) == "rusty-haystack" and wheel_version == version, "wheel filename version/name mismatch")
        require(interpreter == abi == python_tag, "wheel Python/ABI mismatch")
        expected_suffix = "arm64" if target == "aarch64-apple-darwin" else target.split("-")[0]
        platform_tags = platforms.split(".")
        require(all(tag.endswith("_" + expected_suffix) and tag.startswith("macosx_" if "darwin" in target else ("manylinux", "linux_")) for tag in platform_tags), "wheel platform mismatch")
        meta_paths = [p for p in entries if p.endswith(".dist-info/METADATA")]
        wheel_paths = [p for p in entries if p.endswith(".dist-info/WHEEL")]
        require(len(meta_paths) == len(wheel_paths) == 1, "wheel metadata inventory mismatch")
        meta = metadata(entries[meta_paths[0]][0].decode())
        require(canonical(meta["Name"]) == "rusty-haystack" and meta["Version"] == version, "wheel package metadata mismatch")
        require(meta["Requires-Python"] == ">=3.11", "wheel Python floor mismatch")
        wheel = email.parser.Parser().parsestr(entries[wheel_paths[0]][0].decode())
        require(wheel.get_all("Tag") == [f"{interpreter}-{abi}-{tag}" for tag in platform_tags], "wheel WHEEL tags mismatch")
        require(wheel["Generator"] == f"maturin ({MATURIN})" and wheel["Root-Is-Purelib"] == "false", "wheel backend/native metadata mismatch")
        require("rusty_haystack/__init__.pyi" in entries and "rusty_haystack/py.typed" in entries, "missing installed typing files")
        native = [p for p in entries if p.startswith("rusty_haystack/rusty_haystack.") and p.endswith((".so", ".pyd"))]
        require(len(native) == 1 and native_identity(entries[native[0]][0]) == target_identity(target), "wheel native extension target mismatch")
        return {"name": meta["Name"], "version": meta["Version"]}, {"python": interpreter, "abi": abi, "platforms": platform_tags, "requires_python": meta["Requires-Python"]}
    require(kind == "sdist" and target == "source", "unsupported artifact kind")
    roots = {p.split("/")[0] for p in entries}
    require(len(roots) == 1, "source archive must have one root")
    root = next(iter(roots))
    require(f"{root}/PKG-INFO" in entries, "source archive lacks PKG-INFO")
    meta = metadata(entries[f"{root}/PKG-INFO"][0].decode())
    require(canonical(meta["Name"]) == "rusty-haystack" and meta["Version"] == version, "sdist package metadata mismatch")
    require(path.name == f"{root}.tar.gz" and root in (f"rusty_haystack-{version}", f"rusty-haystack-{version}"), "sdist filename mismatch")
    locks = [data for name, (data, _) in entries.items() if name.endswith("/Cargo.lock")]
    require(len(locks) == 1, "source archive lock inventory mismatch")
    return {"name": meta["Name"], "version": meta["Version"]}, {"lock_sha256": hashlib.sha256(locks[0]).hexdigest()}


def lock_prune(before, after):
    old = tomllib.loads(before); new = tomllib.loads(after)
    require({k: v for k, v in old.items() if k != "package"} == {k: v for k, v in new.items() if k != "package"}, "lock metadata changed")
    def packages(lock):
        result = {}
        for item in lock["package"]:
            key = (item["name"], item["version"], item.get("source", ""))
            require(key not in result, "duplicate lock package")
            result[key] = item
        return result
    prior, final = packages(old), packages(new)
    require(final and final.keys() <= prior.keys(), "lock pruning added or upgraded a package")
    require(all(item == prior[key] for key, item in final.items()), "retained lock package changed")
    removed = prior.keys() - final.keys()
    for package in final.values():
        for dep in package.get("dependencies", []):
            parts = dep.split(" ", 2)
            matching = [key for key in prior if key[0] == parts[0] and (len(parts) < 2 or key[1] == parts[1]) and (len(parts) < 3 or key[2] == parts[2].strip("()"))]
            require(len(matching) == 1 and matching[0] not in removed, "lock pruning removed a referenced package")
    return [list(key) for key in sorted(removed)]


def seal(repo, bundle, source, target, kind, tools, *, python_tag=None, removed=None):
    files = bundle / "files"
    require(files.is_dir() and not files.is_symlink(), "missing candidate files")
    inventory = list(files.iterdir())
    require(len(inventory) == 1 and inventory[0].is_file() and not inventory[0].is_symlink(), "each candidate bundle must contain one file")
    path = inventory[0]
    version = project(repo)
    package, details = inspect_file(path, kind, version, target, python_tag)
    receipt = {"schema": 1, "kind": kind, "source": source, "package": package,
               "artifact": {"filename": path.name, "size": path.stat().st_size, "sha256": digest(path)},
               "build": {"target": target, "profile": "sdist" if kind == "sdist" else "release", "features": [] if kind == "cli" else ["pyo3/extension-module"], "tools": tools},
               "details": details}
    if kind == "sdist":
        receipt["removed_lock_packages"] = removed
    write_json(bundle / "receipts" / f"{path.name}.json", receipt)
    return receipt


def expected_tuples(profile, target=None):
    if profile == "binaries":
        return {("cli", value, None) for value in BINARY_TARGETS}
    if profile == "python":
        return {("wheel", value, py) for value in WHEEL_TARGETS for py in PYTHONS} | {("sdist", "source", None)}
    require(profile == "native" and target in WHEEL_TARGETS, "invalid native profile")
    return {("cli", target, None), ("wheel", target, "cp312"), ("sdist", "source", None)}


def read_bundle(bundle, *, source, version, source_lock, allow_dirty=False):
    require({p.name for p in bundle.iterdir()} == {"files", "receipts"}, "unexpected bundle entries")
    files = list((bundle / "files").iterdir()); receipts = list((bundle / "receipts").iterdir())
    require(len(files) == len(receipts) == 1, "missing, duplicate or unexpected candidate files/receipts")
    path, receipt_path = files[0], receipts[0]
    require(all(p.is_file() and not p.is_symlink() for p in (path, receipt_path)) and not any(p.is_symlink() for p in (bundle, bundle / "files", bundle / "receipts")), "candidate symlinks are forbidden")
    require(receipt_path.name == path.name + ".json", "receipt filename mismatch")
    receipt = read_json(receipt_path)
    require(receipt.get("schema") == 1, "unknown receipt schema")
    require(receipt["source"] == {"revision": source, "dirty": False, "lock_sha256": source_lock} or (allow_dirty and receipt["source"] == {"revision": source, "dirty": True, "lock_sha256": source_lock}), "source or lock provenance mismatch")
    require(receipt["artifact"] == {"filename": path.name, "size": path.stat().st_size, "sha256": digest(path)}, "artifact size or digest mismatch")
    build = receipt["build"]
    require(build["profile"] == ("sdist" if receipt["kind"] == "sdist" else "release"), "build profile mismatch")
    require(build["features"] == ([] if receipt["kind"] == "cli" else ["pyo3/extension-module"]), "build feature mismatch")
    require(f"\nrelease: {RUST}\n" in build["tools"]["rustc"] + "\n", "compiler provenance mismatch")
    if receipt["kind"] != "cli":
        require(build["tools"]["maturin"] == f"maturin {MATURIN}", "backend provenance mismatch")
    require(build["tools"]["cargo"].startswith("cargo " + RUST), "Cargo provenance mismatch")
    python_tag = receipt["details"].get("python") if receipt["kind"] == "wheel" else None
    if python_tag:
        python = build["tools"]["python"]
        require(python["implementation"] == "CPython" and python["cache_tag"] == "cpython-" + python_tag[2:] and "cp" + "".join(python["version"].split(".")[:2]) == python_tag, "builder interpreter provenance mismatch")
    package, details = inspect_file(path, receipt["kind"], version, build["target"], python_tag)
    require(receipt["package"] == package and receipt["details"] == details, "package/ABI metadata mismatch")
    return (receipt["kind"], build["target"], python_tag), path, receipt_path, receipt


def inventory(directory, *, profile, target, source, version, source_lock, source_lock_text, allow_dirty=False):
    require(directory.is_dir() and not directory.is_symlink(), "missing downloaded inventory")
    found = {}; filenames = set()
    for bundle in directory.iterdir():
        require(bundle.is_dir() and not bundle.is_symlink(), "unexpected downloaded inventory entry")
        key, path, receipt_path, receipt = read_bundle(bundle, source=source, version=version, source_lock=source_lock, allow_dirty=allow_dirty)
        if key[0] == "sdist":
            shipped = [data.decode() for name, (data, _) in members(path).items() if name.endswith("/Cargo.lock")]
            removed = lock_prune(source_lock_text, shipped[0])
            require(receipt.get("removed_lock_packages") == removed, "source lock transformation receipt mismatch")
        require(key not in found and path.name not in filenames, "duplicate candidate tuple or filename")
        found[key] = (path, receipt_path, receipt); filenames.add(path.name)
    require(set(found) == expected_tuples(profile, target), "missing or unexpected artifact tuple")
    return found


def outside_checkout(path, repo):
    require(not path.resolve().is_relative_to(repo.resolve()), "download/qualification/staging must be outside checkout")


def required_checks(kind, target, py, runtime_target):
    checks = {"provenance", "archive-metadata"}
    if kind == "sdist":
        checks |= {"locked-source-metadata", "pep517-install", "installed-smoke", "typing-layout"}
    elif (kind == "cli" and target == runtime_target):
        checks |= {"cli-version", "cli-fixture"}
    elif kind == "wheel" and target == runtime_target and py == "cp312":
        checks |= {"installed-smoke", "typing-layout"}
    return checks


def stage(found, evidence_dir, destination, runtime_target):
    expected = {path.name + ".json" for path, _, _ in found.values()}
    require({p.name for p in evidence_dir.iterdir()} == expected, "missing or unexpected qualification evidence")
    for key, (path, receipt_path, receipt) in found.items():
        evidence = read_json(evidence_dir / (path.name + ".json"))
        require(evidence.get("schema") == 1 and evidence.get("status") == "passed", "missing successful qualification")
        require(evidence["artifact"] == receipt["artifact"] and evidence["receipt_sha256"] == digest(receipt_path), "qualification does not bind this candidate")
        if required_checks(*key, runtime_target) - {"provenance", "archive-metadata"}:
            require(evidence["environment"]["host"] == runtime_target, "qualification host mismatch")
        require(len(evidence["checks"]) == len(set(evidence["checks"])) and required_checks(*key, runtime_target) <= set(evidence["checks"]), "missing or duplicate qualification check")
    destination.mkdir(parents=True, exist_ok=False)
    for path, _, receipt in found.values():
        with path.open("rb") as original, (destination / path.name).open("xb") as copy:
            shutil.copyfileobj(original, copy)
        require(digest(destination / path.name) == receipt["artifact"]["sha256"], "staged digest mismatch")
    first = next(iter(found.values()))[2]
    return {"status": "staged", "source": first["source"], "version": first["package"]["version"], "tuples": [list(key) for key in sorted(found)], "files": [receipt["artifact"] for _, _, receipt in found.values()]}


def check_staged(directory, record, *, source, version, source_lock, profile, target):
    require(record.get("status") == "staged" and record.get("source") == {"revision": source, "dirty": False, "lock_sha256": source_lock} and record.get("version") == version, "staged provenance mismatch")
    require(record.get("tuples") == [list(key) for key in sorted(expected_tuples(profile, target))], "staged tuple inventory mismatch")
    listed = record["files"]
    require(len(listed) == len(expected_tuples(profile, target)), "staged file count mismatch")
    require(len({item["filename"] for item in listed}) == len(listed), "duplicate staged filename")
    require({path.name for path in directory.iterdir()} == {item["filename"] for item in listed}, "staged file inventory mismatch")
    for item in listed:
        require(safe_name(item["filename"]) == Path(item["filename"]).name, "invalid staged filename")
        path = directory / item["filename"]
        require(path.is_file() and not path.is_symlink() and path.stat().st_size == item["size"] and digest(path) == item["sha256"], "staged digest mismatch")
    return {"status": "verified", "count": len(listed)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    version = commands.add_parser("version", help="parse package version and check a supplied tag")
    version.add_argument("--repo", type=Path, required=True); version.add_argument("--tag")
    tools = commands.add_parser("tools", help="capture actual compiler/backend versions in the builder")
    tools.add_argument("--python"); tools.add_argument("--maturin"); tools.add_argument("--container-info", type=Path); tools.add_argument("--output", type=Path, required=True)
    prune = commands.add_parser("check-lock-prune", help="reject all lock changes except unreachable package removal")
    prune.add_argument("--before", type=Path, required=True); prune.add_argument("--after", type=Path, required=True)
    sealing = commands.add_parser("seal", help="seal one already-built file and its actual builder record")
    sealing.add_argument("--bundle", type=Path, required=True); sealing.add_argument("--kind", choices=("cli", "wheel"), required=True)
    sealing.add_argument("--tools", type=Path, required=True); sealing.add_argument("--target", required=True); sealing.add_argument("--python-tag")
    sealing.add_argument("--repo", type=Path, required=True); sealing.add_argument("--source", required=True)
    for name in ("verify", "stage", "check-staged"):
        item = commands.add_parser(name, help="verify a complete downloaded inventory" if name == "verify" else "copy only qualified files and recheck exact bytes")
        item.add_argument("--repo", type=Path, required=True); item.add_argument("--input", type=Path, required=True)
        item.add_argument("--source", required=True); item.add_argument("--profile", choices=("native", "binaries", "python"), required=True)
        item.add_argument("--target"); item.add_argument("--allow-dirty", action="store_true")
        if name == "check-staged":
            item.add_argument("--receipt", type=Path, required=True)
        if name == "stage":
            item.add_argument("--evidence", type=Path, required=True); item.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        if args.command == "version":
            value = project(args.repo)
            require(args.tag is None or args.tag == "v" + value, "tag does not match parsed package version")
            result = {"version": value}
        elif args.command == "tools":
            result = tool_info(args.python, args.maturin)
            if args.container_info:
                image = read_json(args.container_info)[0]
                require(image["RepoDigests"], "container has no immutable registry digest")
                result["container"] = {"id": image["Id"], "repo_digests": image["RepoDigests"]}
            write_json(args.output, result)
        elif args.command == "check-lock-prune":
            result = {"removed": lock_prune(args.before.read_text(), args.after.read_text())}
        elif args.command == "seal":
            result = seal(args.repo, args.bundle, source_info(args.repo, args.source), args.target, args.kind, read_json(args.tools), python_tag=args.python_tag)
        elif args.command == "check-staged":
            outside_checkout(args.input, args.repo)
            result = check_staged(args.input, read_json(args.receipt), source=args.source, version=project(args.repo), source_lock=digest(args.repo / "Cargo.lock"), profile=args.profile, target=args.target)
        else:
            outside_checkout(args.input, args.repo)
            found = inventory(args.input, profile=args.profile, target=args.target, source=args.source, version=project(args.repo), source_lock=digest(args.repo / "Cargo.lock"), source_lock_text=(args.repo / "Cargo.lock").read_text(), allow_dirty=args.allow_dirty)
            if args.command == "stage":
                outside_checkout(args.output, args.repo)
                result = stage(found, args.evidence, args.output, args.target if args.profile == "native" else "x86_64-unknown-linux-gnu")
            else:
                result = {"status": "verified", "files": [receipt["artifact"] for _, _, receipt in found.values()]}
        print(json.dumps(result, sort_keys=True))
    except (Invalid, OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError, tarfile.TarError, zipfile.BadZipFile) as error:
        print(json.dumps({"status": "failed", "error": str(error)[:512]}), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
