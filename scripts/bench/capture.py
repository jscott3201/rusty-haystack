"""Run the bounded baseline from clean source and retain Criterion samples/provenance.

Usage: python3 scripts/bench/capture.py /absolute/path/outside/the/checkout
Requires a quiet measurement window arranged by the caller. Does not commit files.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[2]
PACKAGES = ("rusty-haystack-core", "rusty-haystack-server")
TOOLCHAIN = "1.99.0"


def check_build_inputs(root, environ):
    """Reject hidden Cargo/compiler inputs without reading configs or secret values."""
    controlled = {"CARGO_BUILD_JOBS", "CARGO_INCREMENTAL", "CARGO_TARGET_DIR"}
    native_names = {"CC", "CXX", "AR", "CFLAGS", "CXXFLAGS", "CPPFLAGS", "LDFLAGS",
                    "RANLIB", "SDKROOT", "DEVELOPER_DIR", "MACOSX_DEPLOYMENT_TARGET",
                    "IPHONEOS_DEPLOYMENT_TARGET"}
    for name in sorted(environ):
        cargo_override = name.startswith("CARGO_") and name not in controlled | {"CARGO_HOME"}
        rust_override = name.startswith(("RUSTC", "RUSTDOC")) or name in {
            "RUST_TARGET_PATH", "RUSTUP_TOOLCHAIN",
        }
        native_override = name in native_names or name.startswith(
            tuple(f"{prefix}_" for prefix in native_names | {"HOST", "TARGET"})
        )
        if cargo_override or rust_override or native_override:
            raise ValueError(f"remove the unrecorded build override {name}")

    home_name = "USERPROFILE" if os.name == "nt" else "HOME"
    home = Path(environ.get(home_name) or Path.home())
    cargo_home = Path(environ.get("CARGO_HOME") or home / ".cargo")
    if not cargo_home.is_absolute():
        cargo_home = root / cargo_home
    directories = {directory / ".cargo" for directory in (root, *root.parents)}
    directories.add(cargo_home)
    for directory in directories:
        for filename in ("config", "config.toml"):
            try:
                (directory / filename).lstat()
            except FileNotFoundError:
                continue
            except OSError:
                raise ValueError("cannot establish absence of Cargo configuration") from None
            raise ValueError("active Cargo configuration is unsupported; use a checkout and "
                             "Cargo home without config/config.toml (contents were not read)")

    manifest = root / "Cargo.toml"
    profiles = tomllib.loads(manifest.read_text()).get("profile", {}) if manifest.exists() else {}
    return {"cargo_config_files": [], "workspace_profiles": profiles}


def output(*command):
    return subprocess.check_output(command, cwd=ROOT, text=True).strip()


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def toolchain():
    executables = {}
    for name in ("cargo", "rustc", "rustdoc"):
        path = Path(output("rustup", "which", "--toolchain", TOOLCHAIN, name)).resolve(strict=True)
        executables[name] = {"path": str(path), "sha256": digest(path)}
    rustc = output(executables["rustc"]["path"], "-Vv")
    host = [line.removeprefix("host: ") for line in rustc.splitlines() if line.startswith("host: ")]
    if len(host) != 1:
        raise ValueError("cannot establish the selected compiler host target")
    return {"executables": executables, "rustc": rustc,
            "cargo": output(executables["cargo"]["path"], "--version"), "host_target": host[0]}


def source():
    return {"commit": output("git", "rev-parse", "HEAD"),
            "dirty": bool(output("git", "status", "--porcelain", "--untracked-files=normal")),
            "cargo_lock_sha256": digest(ROOT / "Cargo.lock")}


def expected_ids():
    core = {f"core_baseline/{query}/{state}/{size}"
            for query in ("marker", "reference", "nested_reference")
            for state in ("cold", "ast_warm_result_cold", "result_warm")
            for size in (972, 10125)}
    core |= {f"core_baseline/mutation_then_nested_query/{size}" for size in (972, 10125)}
    return core | {f"history_baseline/{query}/{size}"
                   for query in ("direct_full", "direct_first_day", "http_first_day")
                   for size in (1000, 10000)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output_directory", type=Path)
    args = parser.parse_args()
    destination = args.output_directory.resolve()
    if destination == ROOT or ROOT in destination.parents:
        parser.error("write raw output outside the checkout to preserve clean source")
    if destination.exists() and any(destination.iterdir()):
        parser.error("output directory must be absent or empty")
    try:
        build_inputs = check_build_inputs(ROOT, os.environ)
    except ValueError as error:
        parser.error(str(error))
    before = source()
    if before["dirty"]:
        parser.error("measurement requires a clean, committed source tree")
    selected_toolchain = toolchain()

    destination.mkdir(parents=True, exist_ok=True)
    criterion_home = destination / "criterion"
    env = os.environ.copy()
    settings = {"RUSTFLAGS": "-Dwarnings", "CARGO_BUILD_JOBS": "2",
                "CARGO_INCREMENTAL": "0", "CARGO_TARGET_DIR": str(ROOT / "target"),
                "RUSTC": selected_toolchain["executables"]["rustc"]["path"],
                "RUSTDOC": selected_toolchain["executables"]["rustdoc"]["path"],
                "CRITERION_HOME": str(criterion_home)}
    env.update(settings)
    lock = tomllib.loads((ROOT / "Cargo.lock").read_text())
    criterion_version = next(package["version"] for package in lock["package"]
                             if package["name"] == "criterion")
    host = {"os": platform.platform(), "machine": platform.machine(), "logical_cpus": os.cpu_count()}
    if sys.platform == "darwin":
        host.update(cpu=output("sysctl", "-n", "machdep.cpu.brand_string"),
                    memory_bytes=int(output("sysctl", "-n", "hw.memsize")))
    record = {
        "schema_version": 2, "status": "running", "source_before": before,
        "started_utc": datetime.now(timezone.utc).isoformat(), "host": host,
        "toolchain": selected_toolchain, "build_inputs": build_inputs,
        "target": "compiler host; no --target flag or inherited Cargo target selection",
        "profile": "Cargo bench defaults plus recorded workspace manifest profiles",
        "criterion_version": criterion_version,
        "features": {"packages": "default features; no --features flags", "criterion": "default + html_reports"},
        "environment": settings,
        "criterion_configuration": {"sample_size": 30, "warmup_seconds": 0.5,
                                    "measurement_seconds": 2, "resamples": 10000, "confidence_level": 0.95},
        "workers": {"core": 1, "history_direct": 1, "history_http_tokio_workers": 2,
                    "history_http_calling_threads": 1, "http_requests_in_flight": 1},
        "timing_units": "nanoseconds", "memory_rss_measured": False,
        "commands": [], "fixture_metadata": [], "measurements": [],
    }
    record_path = destination / "baseline.json"
    try:
        for package in PACKAGES:
            if check_build_inputs(ROOT, os.environ) != build_inputs:
                raise RuntimeError("build inputs changed during measurement")
            command = [selected_toolchain["executables"]["cargo"]["path"],
                       "bench", "--locked", "-p", package,
                       "--bench", "baseline", "--", "--noplot", "--save-baseline", "m0"]
            log_path = destination / f"{package}.log"
            print("Running " + " ".join(command), flush=True)
            with log_path.open("w") as log:
                result = subprocess.run(command, cwd=ROOT, env=env, stdout=log,
                                        stderr=subprocess.STDOUT, check=False)
            record["commands"].append({"argv": command, "exit_code": result.returncode,
                                       "log": log_path.name, "log_sha256": digest(log_path)})
            record["fixture_metadata"].extend(line for line in log_path.read_text().splitlines()
                                               if line.startswith(("dataset=", "query=", "mutation=")))
            if result.returncode:
                raise RuntimeError(f"benchmark failed; inspect {log_path}")
        for path in sorted(criterion_home.rglob("m0/estimates.json")):
            directory = path.parent
            record["measurements"].append({
                "benchmark": json.loads((directory / "benchmark.json").read_text()),
                "estimates": json.loads(path.read_text()),
                "sample": json.loads((directory / "sample.json").read_text()),
            })
        observed = {entry["benchmark"]["full_id"] for entry in record["measurements"]}
        if observed != expected_ids() or len(record["measurements"]) != 26:
            raise RuntimeError("Criterion output does not contain the exact 26-workload inventory")
        record["source_after"] = source()
        if record["source_after"] != before:
            raise RuntimeError("source or lockfile changed during measurement")
        if check_build_inputs(ROOT, os.environ) != build_inputs or toolchain() != selected_toolchain:
            raise RuntimeError("build inputs or selected toolchain changed during measurement")
        record["status"] = "complete"
    except BaseException:
        record["status"] = "failed"
        raise
    finally:
        record["finished_utc"] = datetime.now(timezone.utc).isoformat()
        record_path.write_text(json.dumps(record, indent=2) + "\n")
    print(f"Saved {len(record['measurements'])} workloads to {record_path}", flush=True)


if __name__ == "__main__":
    main()
