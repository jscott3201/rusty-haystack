#!/usr/bin/env python3
"""Verify retained bytes and reproduce the documented pinned source extraction.

No network or third-party Python dependencies. With --output DIR, write the five
selected sources for inspection; without it, only verify and report their hashes.
The Rust admission loader reproduces these selections directly from embedded raw
files, so generated snippets are not a second runtime authority.
"""

import argparse
import hashlib
import json
from pathlib import Path
import re

PIN = "873b922451d3ef4c0c9c08ef3daa542f352d69f3"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--http", action="store_true", help="verify the expanded executable HTTP closure")
    args = parser.parse_args()
    root = Path(__file__).resolve().parent
    manifest = json.loads((root / ("http-manifest.json" if args.http else "manifest.json")).read_text())
    assert manifest["commit"] == PIN
    assert manifest["complete_libraries"] is False
    raw = {}
    for entry in manifest["files"]:
        data = (root / "upstream" / entry["path"]).read_bytes()
        if hashlib.sha256(data).hexdigest() != entry["sha256"]:
            raise SystemExit(f"SHA-256 mismatch: {entry['path']}")
        raw[entry["path"]] = data.decode("utf-8")
    variables = {}
    for line in raw["src/xeto/xeto-build.props"].splitlines():
        if line.strip() and not line.lstrip().startswith("//"):
            key, value = line.split("=", 1)
            if key in variables:
                raise SystemExit(f"duplicate build property: {key}")
            variables[key] = value

    def expand(match):
        name = match.group(1)
        if name not in variables:
            raise SystemExit(f"unresolved build variable: {name}")
        return json.dumps(variables[name], ensure_ascii=False)

    if args.output:
        args.output.mkdir(parents=True, exist_ok=True)
    selected = 0
    for entry in manifest["files"]:
        if entry["role"] in ("license", "build", "evidence"):
            continue
        lines = raw[entry["path"]].splitlines()
        pieces = []
        for first, last in entry["lines"]:
            if not 1 <= first <= last <= len(lines):
                raise SystemExit(f"invalid extraction range: {entry['path']}")
            pieces.append("\n".join(lines[first - 1:last]) + "\n\n")
        text = re.sub(r'BuildVar\s+"([^"]+)"', expand, "".join(pieces))
        digest = hashlib.sha256(text.encode()).hexdigest()
        name = f"{entry['library']}-{entry['role']}.xeto"
        if args.output:
            (args.output / name).write_text(text)
        print(f"{digest}  {name}")
        selected += 1
    print(f"Verified {len(raw)} unchanged upstream files; reproduced {selected} source selections.")


if __name__ == "__main__":
    main()
