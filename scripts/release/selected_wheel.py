#!/usr/bin/env python3
"""Qualify one selected native wheel; this command never publishes or rebuilds it."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys

import artifacts as a
from build import run
from runtime_profile import SELECTED_ABIS, validate as validate_runtime

PROCESSES = {"positive-runtime", "async-runtime", "application-runtime", "watch-runtime", "h5-runtime", "sync-runtime", "concurrency-runtime"}
IMPORTS = ("rusty_haystack", "rusty_haystack.rusty_haystack", "rusty_haystack.kinds", "rusty_haystack.data", "rusty_haystack.codecs", "rusty_haystack.filter", "rusty_haystack.units", "rusty_haystack.graph", "rusty_haystack.ontology", "rusty_haystack.auth", "rusty_haystack.client", "rusty_haystack.server")
DEPENDENCIES = ("asyncio", "concurrent.futures", "threading", "ssl", "http.server")
CAPABILITIES = {
    "typed-values": "positive-runtime", "async-h4": "async-runtime", "async-h5": "h5-runtime",
    "bounded-watches": "watch-runtime", "embedded-memory": "application-runtime",
    "sync-facade": "sync-runtime", "concurrency": "concurrency-runtime",
}
UNSELECTED = ("postgresql", "sqlite", "mcp", "analytics", "free-threaded-sdist", "other-interpreters-and-platforms")
MACOS_PYTHONS = {"cp312": "3.12.13", "cp314t": "3.14.4"}
REQUIRED_FLAGS = {
    "async_runtime": ("http_scram_read", "cancelled_read_peer_eof", "custom_ca_tls_read", "closed_owners"),
    "h5_runtime": ("first_party_http", "rich_values", "ordered_absence", "typed_error", "closed_owners"),
    "application_runtime": ("embedded_authenticated_http_parity", "anonymous_and_no_read_denied", "native_pagination", "accepted_socket_eof", "exact_port_reused", "borrowed_graph_and_bridge_usable", "cancelled_entry_after_completed_future"),
    "watch_runtime": ("direct_real_router_ws", "real_server_typed_progression", "explicit_ack_resume_renew", "single_session_transcript", "owned_child_and_parent_cleanup", "attach_failure_cleanup", "key_ownership_and_cleanup_budget"),
    "sync_runtime": ("direct_values", "exit_keywords", "python_peer_progress", "loop_thread_rejection", "read_timeout_recovery", "custom_ca_tls", "closed_owners", "legacy_capabilities_preserved"),
    "concurrency_runtime": ("atomic_tag_snapshots", "returned_copy_independent", "close_during_reads"),
}


def wheel_members(path):
    return {name: hashlib.sha256(data).hexdigest() for name, (data, _) in a.members(path).items() if name.startswith("rusty_haystack/")}


def capabilities():
    return {**{name: {"status": "passed", "process": process} for name, process in CAPABILITIES.items()},
            **{name: {"status": "unselected"} for name in UNSELECTED}}


def selected_interpreter(target, abi, observed):
    a.require(target in a.SELECTED_TARGETS, "unselected native host")
    validate_runtime(observed, abi)
    machine = "arm64" if target == "aarch64-apple-darwin" else "x86_64"
    a.require(observed["machine"] == machine, "selected interpreter architecture mismatch")
    if target == "aarch64-apple-darwin":
        a.require(observed["version"] == MACOS_PYTHONS[abi], "unselected macOS interpreter patch version")


def validate_result(result, receipt, path, fixture):
    abi = receipt["details"]["abi"]
    a.require(result["abi"] == abi and result["wheel_sha256"] == receipt["artifact"]["sha256"] == a.digest(path), "consumer wheel identity mismatch")
    expected_members = wheel_members(path)
    expected_machine = "arm64" if receipt["build"]["target"] == "aarch64-apple-darwin" else "x86_64"
    for installed in (result["installed"], result["installed_after"]):
        a.require(installed["wheel_members_sha256"] == expected_members, "installed member digest mismatch")
        a.require(installed["typing_files_sha256"] == receipt["details"]["typing_files_sha256"], "installed typing mismatch")
        a.require(installed["version"] == receipt["package"]["version"], "installed package version mismatch")
        a.require(installed["python"] == receipt["build"]["tools"]["python"]["version"], "builder and consumer interpreter version mismatch")
        for name in ("module_origin", "native_origin"):
            a.require(Path(installed[name]).is_relative_to(installed["prefix"]), "source import contamination")
        for name in ("runtime_before", "runtime_after"):
            validate_runtime(installed[name], abi)
            a.require(installed[name]["machine"] == expected_machine, "runtime architecture mismatch")
    a.require(result["installed"] == result["installed_after"], "installed package changed during qualification")
    a.require(set(result["runtime_modes"]) == PROCESSES, "missing capability process evidence")
    phases = ["before-package", *("import:" + name for name in IMPORTS), *("dependency:" + name for name in DEPENDENCIES), "after-consumer"]
    for label, process in result["runtime_modes"].items():
        a.require(process["status"] == "passed" and process["abi"] == abi and process["pid"] > 0, "capability process did not pass")
        a.require(process["script"] == ("positive.py" if label == "positive-runtime" else label + ".py"), "capability process identity mismatch")
        a.require([item["phase"] for item in process["observations"]] == phases, "missing import or terminal runtime observation")
        for item in process["observations"]:
            validate_runtime(item["runtime"], abi)
            a.require(item["runtime"]["machine"] == expected_machine, "capability runtime architecture mismatch")
            a.require(item["runtime"]["version"] == result["installed"]["python"], "capability interpreter changed")
    for section, flags in REQUIRED_FLAGS.items():
        a.require(all(result[section].get(flag) is True for flag in flags), "missing successful capability: " + section)
    concurrency = result["concurrency_runtime"]
    a.require(concurrency["shared_graph_final_count"] == 201 and concurrency["create_destroy_cycles"] == 12, "missing bounded concurrency evidence")
    for key in ("source_dict_constructor", "source_dict_factory", "source_list"):
        a.require(concurrency[key]["conversions"] == 100 and concurrency[key]["mutations"] > 0, "source mutation was not exercised")
    a.require(result["checker"] == "pyright 1.1.413" and result["positive_errors"] == 0 and result["negative_lines_rejected"] == 79, "missing installed static evidence")
    a.require(result["watch_fixture_build"] == fixture and result["watch_runtime"]["fixture_sha256"] == fixture["sha256"], "watch fixture identity mismatch")


def native_linkage(path, target, tags):
    a.require(a.native_identity(path.read_bytes()) == a.target_identity(target), "installed native architecture mismatch")
    if "darwin" in target:
        libraries = a.output(["otool", "-L", str(path)])
        commands = a.output(["otool", "-l", str(path)])
        dependencies = [line.strip().split(" (", 1)[0] for line in libraries.splitlines()[1:]]
        a.require(dependencies and dependencies[0] == "@rpath/" + path.name, "unexpected native install identity")
        dependencies = dependencies[1:]
        a.require(dependencies and all(name.startswith(("/usr/lib/", "/System/Library/")) for name in dependencies), "non-system native dependency")
        a.require(not any("libpython" in name.lower() for name in dependencies), "wheel must not link libpython")
        builds = re.findall(r"cmd LC_BUILD_VERSION\s+cmdsize \d+\s+platform (\d+)\s+minos ([\d.]+)\s+sdk ([\d.]+)", commands)
        a.require(len(builds) == 1 and builds[0][0] == "1", "missing native macOS deployment requirement")
        minimum = tuple(map(int, builds[0][1].split(".")))
        for tag in tags:
            tagged = re.fullmatch(r"macosx_(\d+)_(\d+)_arm64", tag)
            a.require(tagged is not None and tuple(map(int, tagged.groups())) >= minimum[:2], "wheel understates native deployment requirement")
        rpaths = re.findall(r"cmd LC_RPATH\s+cmdsize \d+\s+path (.*?) \(offset", commands)
        a.require(not rpaths, "unexpected native search path")
        return {"status": "passed", "format": "Mach-O", "native_sha256": a.digest(path), "dependencies": dependencies, "minimum_macos": builds[0][1], "sdk": builds[0][2], "rpaths": rpaths, "inspection": libraries, "build_commands_sha256": hashlib.sha256(commands.encode()).hexdigest()}
    dynamic = a.output(["readelf", "--dynamic", "--wide", str(path)])
    versions = a.output(["readelf", "--version-info", "--wide", str(path)])
    dependencies = re.findall(r"\(NEEDED\).*?\[(.*?)\]", dynamic)
    allowed = {"libc.so.6", "libm.so.6", "libgcc_s.so.1", "libpthread.so.0", "libdl.so.2", "librt.so.1", "libstdc++.so.6", "ld-linux-x86-64.so.2"}
    a.require(dependencies and set(dependencies) <= allowed, "non-system native dependency")
    a.require(not re.search(r"\((RPATH|RUNPATH)\)", dynamic), "unexpected native search path")
    required_glibc = {tuple(map(int, value.split("."))) for value in re.findall(r"Name: GLIBC_([\d.]+)", versions)}
    a.require(required_glibc, "missing native glibc requirements")
    minimum = max(required_glibc)
    legacy_floors = {"manylinux1_x86_64": (2, 5), "manylinux2010_x86_64": (2, 12), "manylinux2014_x86_64": (2, 17)}
    for tag in tags:
        if tag == "linux_x86_64":
            continue
        tagged = re.fullmatch(r"manylinux_(\d+)_(\d+)_x86_64", tag)
        floor = tuple(map(int, tagged.groups())) if tagged else legacy_floors.get(tag)
        a.require(floor is not None, "unrecognized native Linux platform tag")
        a.require(floor >= minimum, "wheel understates native glibc requirement")
    return {"status": "passed", "format": "ELF", "native_sha256": a.digest(path), "dependencies": dependencies, "minimum_glibc": ".".join(map(str, minimum)), "host_libc": list(platform.libc_ver()), "inspection": dynamic, "version_requirements": versions}


def fixture_build(args):
    repo, work = args.repo.resolve(), args.work.resolve()
    a.outside_checkout(work, repo); a.outside_checkout(args.output, repo)
    source = a.source_info(repo, args.source)
    tools = a.tool_info(repo=repo, target=args.target)
    a.require(args.target in a.SELECTED_TARGETS and "host: " + args.target in tools["rustc"].splitlines(), "fixture requires selected native host")
    work.mkdir(parents=True, exist_ok=False)
    command = [tools["executables"]["cargo"]["path"], "build", "--locked", "-p", "rusty-haystack-server", "--example", "python_watch_fixture"]
    run(command, work, cwd=repo, env=a.build_environment(tools), label="fixture-build")
    binary = Path(tools["context"]["target_dir"]) / "debug/examples/python_watch_fixture"
    a.require(a.native_identity(binary.read_bytes()) == a.target_identity(args.target), "fixture architecture mismatch")
    a.require(a.source_info(repo, args.source) == source and a.tool_info(repo=repo, target=args.target) == tools, "fixture build context changed")
    result = {"schema": 1, "status": "built", "source": source, "target": args.target, "path": str(binary), "sha256": a.digest(binary),
              "profile": "dev", "command": command, "tools": tools, "input": {"haystack-server/examples/python_watch_fixture.rs": a.digest(repo / "haystack-server/examples/python_watch_fixture.rs")}}
    a.write_json(args.output, result)
    return {"status": "built", "path": str(binary), "sha256": result["sha256"]}


def inputs(args):
    repo, incoming = args.repo.resolve(), args.input.resolve()
    a.outside_checkout(incoming, repo)
    source = a.source_info(repo, args.source)
    a.require(args.target in a.SELECTED_TARGETS, "unselected native host")
    bundles = [incoming] if (incoming / "files").is_dir() else list(incoming.iterdir())
    a.require(len(bundles) == 1, "selected qualification requires exactly one candidate")
    key, path, receipt_path, receipt = a.read_bundle(bundles[0], source=args.source, version=a.project(repo), source_lock=source["lock_sha256"], source_typing=a.typing_sources(repo))
    a.require(key == ("wheel", args.target, args.abi), "selected candidate tuple mismatch")
    selected_interpreter(args.target, args.abi, receipt["build"]["tools"]["python"])
    a.require(re.fullmatch(r"[0-9a-f]{64}", receipt["build"]["tools"]["python"]["executable_sha256"]), "missing builder interpreter digest")
    fixture = a.read_json(args.fixture_receipt)
    a.require(fixture["schema"] == 1 and fixture["status"] == "built" and fixture["source"] == source and fixture["target"] == args.target, "fixture source or target mismatch")
    a.require(fixture["input"] == {"haystack-server/examples/python_watch_fixture.rs": a.digest(repo / "haystack-server/examples/python_watch_fixture.rs")}, "fixture source input mismatch")
    a.require(fixture["profile"] == "dev" and fixture["tools"]["target"] == args.target and "\nrelease: " + a.RUST + "\n" in fixture["tools"]["rustc"] + "\n", "fixture compiler context mismatch")
    fixture_path = Path(fixture["path"])
    a.require(a.digest(fixture_path) == fixture["sha256"], "fixture binary digest mismatch")
    return repo, source, path, receipt_path, receipt, fixture


def qualify(args):
    repo, source, path, receipt_path, receipt, fixture = inputs(args)
    work = args.work.resolve()
    for output in (work, args.evidence): a.outside_checkout(output, repo)
    fixture_path = Path(fixture["path"])
    work.mkdir(parents=True, exist_ok=False)
    command = [sys.executable, str(repo / "scripts/python-values/validate.py"), "--wheel", str(path), "--python", args.python,
               "--abi", args.abi, "--pyright", args.pyright, "--watch-fixture", str(fixture_path), "--fixture-receipt", str(args.fixture_receipt.resolve()), "--work", str(work / "consumer")]
    run(command, work, cwd=work, label="installed-consumer")
    result = a.read_json(work / "consumer/result.json")
    a.require(result["consumer_inputs"] == a.python_consumer_inputs(repo), "consumer source input mismatch")
    validate_result(result, receipt, path, fixture)
    linkage = native_linkage(Path(result["installed"]["native_origin"]), args.target, receipt["details"]["platforms"])
    a.require(a.source_info(repo, args.source) == source and a.digest(fixture_path) == fixture["sha256"] and a.digest(path) == receipt["artifact"]["sha256"], "source or input changed during qualification")
    evidence = {"schema": 1, "profile": "selected-wheel", "status": "passed", "source": source, "target": args.target,
                "artifact": receipt["artifact"], "receipt_sha256": a.digest(receipt_path), "capabilities": capabilities(),
                "consumer": result, "native_linkage": linkage, "environment": {"os": platform.platform(), "machine": platform.machine()},
                "fixture_receipt_sha256": a.digest(args.fixture_receipt)}
    verify_evidence(evidence, receipt, receipt_path, path, fixture, a.digest(args.fixture_receipt))
    a.write_json(args.evidence, evidence)
    return {"status": "qualified", "abi": args.abi, "artifact": receipt["artifact"], "capabilities": evidence["capabilities"]}


def verify_evidence(evidence, receipt, receipt_path, path, fixture, fixture_receipt_sha256):
    a.require(evidence["schema"] == 1 and evidence["profile"] == "selected-wheel" and evidence["status"] == "passed", "missing selected qualification")
    a.require(evidence["source"] == receipt["source"] and evidence["target"] == receipt["build"]["target"], "qualification source or target mismatch")
    a.require(evidence["artifact"] == receipt["artifact"] and evidence["receipt_sha256"] == a.digest(receipt_path), "qualification receipt mismatch")
    a.require(evidence["capabilities"] == capabilities(), "missing or unselected capability result")
    a.require(evidence["fixture_receipt_sha256"] == fixture_receipt_sha256, "fixture receipt digest mismatch")
    a.require(evidence["native_linkage"]["status"] == "passed", "missing native linkage evidence")
    native = "rusty_haystack/" + Path(evidence["consumer"]["installed"]["native_origin"]).name
    a.require(evidence["native_linkage"]["native_sha256"] == wheel_members(path)[native], "linkage does not bind installed native module")
    expected_format, minimum = ("Mach-O", "minimum_macos") if "darwin" in receipt["build"]["target"] else ("ELF", "minimum_glibc")
    linkage = evidence["native_linkage"]
    a.require(linkage["format"] == expected_format and re.fullmatch(r"[0-9]+(?:\.[0-9]+)+", linkage[minimum]) and linkage["dependencies"], "missing native deployment requirements")
    validate_result(evidence["consumer"], receipt, path, fixture)


def check(args):
    repo, _, path, receipt_path, receipt, fixture = inputs(args)
    evidence = a.read_json(args.evidence)
    a.require(evidence["consumer"]["consumer_inputs"] == a.python_consumer_inputs(repo), "consumer source input mismatch")
    verify_evidence(evidence, receipt, receipt_path, path, fixture, a.digest(args.fixture_receipt))
    installed = evidence["consumer"]["installed"]
    package_parent = Path(installed["module_origin"]).parent.parent
    for name, digest in wheel_members(path).items():
        a.require(a.digest(package_parent / name) == digest, "installed package changed after qualification")
    a.require(native_linkage(Path(installed["native_origin"]), args.target, receipt["details"]["platforms"]) == evidence["native_linkage"], "native linkage evidence changed")
    return {"status": "verified", "artifact": receipt["artifact"], "abi": args.abi}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    subcommands = parser.add_subparsers(dest="command", required=True)
    for name in ("fixture", "qualify", "check"):
        item = subcommands.add_parser(name)
        item.add_argument("--repo", type=Path, required=True); item.add_argument("--source", required=True)
        item.add_argument("--target", required=True)
        if name != "check": item.add_argument("--work", type=Path, required=True)
        if name == "fixture":
            item.add_argument("--output", type=Path, required=True)
        else:
            item.add_argument("--input", type=Path, required=True); item.add_argument("--abi", choices=SELECTED_ABIS, required=True)
            if name == "qualify":
                item.add_argument("--python", required=True); item.add_argument("--pyright", required=True)
            item.add_argument("--fixture-receipt", type=Path, required=True); item.add_argument("--evidence", type=Path, required=True)
    args = parser.parse_args()
    try:
        print(json.dumps({"fixture": fixture_build, "qualify": qualify, "check": check}[args.command](args), sort_keys=True))
    except (a.Invalid, OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        print(json.dumps({"status": "failed", "error": str(error)[:512]}), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
