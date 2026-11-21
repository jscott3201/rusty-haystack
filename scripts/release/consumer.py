#!/usr/bin/env python3
"""Run with the isolated artifact interpreter, from a scratch working directory."""
import argparse
import ast
import hashlib
import hmac
import importlib
import importlib.metadata
import json
from pathlib import Path
import platform
import runpy
import sys

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--prefix", type=Path, required=True)
parser.add_argument("--version", required=True)
parser.add_argument("--typing-manifest", type=Path, required=True)
parser.add_argument("--abi", choices=("cp312", "cp314t"), default="cp312")
parser.add_argument("--wheel-manifest", type=Path)
args = parser.parse_args()
profile = runpy.run_path(str(Path(__file__).with_name("runtime_profile.py")))
before = profile["observe"]()
profile["validate"](before, args.abi)
assert Path(sys.prefix).resolve() == args.prefix.resolve()
module = importlib.import_module("rusty_haystack")
origin = Path(module.__file__).resolve()
assert origin.is_relative_to(args.prefix.resolve()), origin
assert not Path.cwd().is_relative_to(args.prefix.resolve())
assert module.__version__ == importlib.metadata.version("rusty-haystack") == args.version
for name in ("kinds", "data", "codecs", "filter", "units", "graph", "ontology", "auth", "client", "server"):
    assert importlib.import_module("rusty_haystack." + name) is getattr(module, name)
number = module.decode_scalar("application/json;v=3", '"n:72.5 °F"')
assert isinstance(number, module.Number) and number.val == 72.5 and number.unit == "°F"
assert module.encode_scalar("application/json;v=3", module.Ref("artifact-point")) == '"r:artifact-point"'
grid = module.decode_grid("text/zinc", 'ver:"3.0"\nid,dis,point\n@artifact-point,"Installed artifact",M\n')
assert len(grid) == 1 and grid[0]["id"].val == "artifact-point" and grid[0]["dis"] == "Installed artifact"
try:
    module.decode_grid("text/unknown", "")
except module.CodecError:
    pass
else:
    raise AssertionError("public codec exception missing")
salt = b"artifact-fixture"
stored, server = module.auth.derive_credentials("fixed-test-password", salt, 4096)
salted = hashlib.pbkdf2_hmac("sha256", b"fixed-test-password", salt, 4096)
assert stored == hashlib.sha256(hmac.digest(salted, b"Client Key", "sha256")).digest()
assert server == hmac.digest(salted, b"Server Key", "sha256")
package = origin.parent
stub = package / "__init__.pyi"
marker = package / "py.typed"
assert stub.is_file() and marker.is_file()
expected_typing = json.loads(args.typing_manifest.read_text())
assert set(expected_typing) == {p.name for p in package.glob("*.pyi")} | {"py.typed"}
for name, expected in expected_typing.items():
    assert hashlib.sha256((package / name).read_bytes()).hexdigest() == expected, name
classes = {node.name for node in ast.parse(stub.read_text()).body if isinstance(node, ast.ClassDef)}
assert {"Number", "Ref", "HGrid", "CodecError", "ClientError"} <= classes
installed_files = {str(path) for path in importlib.metadata.distribution("rusty-haystack").files}
assert {"rusty_haystack/" + name for name in expected_typing} <= installed_files
wheel_members = {}
if args.wheel_manifest:
    wheel_members = json.loads(args.wheel_manifest.read_text())
    assert wheel_members and set(wheel_members) <= installed_files
    for name, expected in wheel_members.items():
        path = package.parent / name
        assert path.resolve().is_relative_to(args.prefix.resolve())
        assert hashlib.sha256(path.read_bytes()).hexdigest() == expected, name
native = importlib.import_module("rusty_haystack.rusty_haystack")
native_origin = Path(native.__file__).resolve()
assert native_origin.is_relative_to(args.prefix.resolve())
after = profile["observe"]()
profile["validate"](after, args.abi)
print(json.dumps({"implementation": platform.python_implementation(), "python": platform.python_version(), "prefix": str(args.prefix.resolve()), "module_origin": str(origin), "native_origin": str(native_origin), "version": module.__version__, "typing_files_sha256": expected_typing, "wheel_members_sha256": wheel_members, "runtime_before": before, "runtime_after": after, "typing": "installed PEP 561 layout, complete current stub digests and parsed public names; no full type-checker claim"}, sort_keys=True))
