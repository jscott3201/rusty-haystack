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
import sys

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--prefix", type=Path, required=True)
parser.add_argument("--version", required=True)
parser.add_argument("--stub-sha256", required=True)
args = parser.parse_args()
assert platform.python_implementation() == "CPython" and sys.version_info[:2] == (3, 12)
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
assert hashlib.sha256(stub.read_bytes()).hexdigest() == args.stub_sha256
classes = {node.name for node in ast.parse(stub.read_text()).body if isinstance(node, ast.ClassDef)}
assert {"Number", "Ref", "HGrid", "CodecError", "ClientError"} <= classes
installed_files = {str(path) for path in importlib.metadata.distribution("rusty-haystack").files}
assert {"rusty_haystack/__init__.pyi", "rusty_haystack/py.typed"} <= installed_files
print(json.dumps({"implementation": platform.python_implementation(), "python": platform.python_version(), "prefix": str(args.prefix.resolve()), "module_origin": str(origin), "version": module.__version__, "typing_stub_sha256": args.stub_sha256, "typing": "installed PEP 561 layout, current stub digest and parsed public names; no full type-checker claim"}, sort_keys=True))
