"""Run one installed capability consumer with same-process runtime evidence."""
import argparse
import importlib
import json
import os
from pathlib import Path
import runpy
import sys

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--abi", required=True)
parser.add_argument("--prefix", type=Path, required=True)
parser.add_argument("--evidence", type=Path, required=True)
parser.add_argument("script", type=Path)
parser.add_argument("arguments", nargs=argparse.REMAINDER)
args = parser.parse_args()
profile = runpy.run_path(str(Path(__file__).with_name("runtime_profile.py")))
observations = []


def record(phase):
    observation = profile["observe"]()
    observations.append({"phase": phase, "runtime": observation})
    profile["validate"](observation, args.abi)


status = "failed"
try:
    assert Path(sys.prefix).resolve() == args.prefix.resolve()
    record("before-package")
    for name in ("rusty_haystack", "rusty_haystack.rusty_haystack", "rusty_haystack.kinds", "rusty_haystack.data", "rusty_haystack.codecs", "rusty_haystack.filter", "rusty_haystack.units", "rusty_haystack.graph", "rusty_haystack.ontology", "rusty_haystack.auth", "rusty_haystack.client", "rusty_haystack.server"):
        module = importlib.import_module(name)
        if getattr(module, "__file__", None):
            assert Path(module.__file__).resolve().is_relative_to(args.prefix.resolve()), name
        record("import:" + name)
    for name in ("asyncio", "concurrent.futures", "threading", "ssl", "http.server"):
        importlib.import_module(name)
        record("dependency:" + name)
    sys.argv = [str(args.script), *args.arguments]
    runpy.run_path(str(args.script), run_name="__main__")
    record("after-consumer")
    status = "passed"
finally:
    args.evidence.write_text(json.dumps({"status": status, "pid": os.getpid(), "script": args.script.name, "abi": args.abi, "observations": observations}, indent=2, sort_keys=True) + "\n")
