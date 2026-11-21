#!/usr/bin/env python3
"""Qualify an installed MCP CLI with independent JSON Schema and pipe observers."""
import argparse
import array
import fcntl
import json
import os
from pathlib import Path
import queue
import signal
import select
import subprocess
import tempfile
import threading
import termios
import time

from jsonschema import Draft202012Validator

ROOT = Path(__file__).resolve().parents[2]


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def fixture(directory, *, large=False, timeout=2000):
    dataset = directory / "entities.trio"
    dataset.write_text('id: @fixture-a\nsite\ndis: "Fixture Site"\nsecret: "DO_NOT_DISCLOSE"\n'
                       + ('large: "' + 'x' * 50000 + '"\n' if large else '')
                       + '\n---\nid: @hidden\nsite\ndis: "Hidden"\n')
    config = directory / "mcp.toml"
    config.write_text(f'version = 1\ndataset_file = "entities.trio"\ndataset_format = "trio"\n'
                      'principal = "fixture-reader"\nentity_ids = ["fixture-a"]\nhidden_tags = ["secret"]\n'
                      f'request_timeout_ms = {timeout}\n')
    return config


class Session:
    def __init__(self, binary, config, *, response_limit=1024 * 1024):
        self.child = subprocess.Popen([str(binary), "mcp", "stdio", "--config", str(config)],
                                      stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.messages = queue.Queue(maxsize=32)
        self.reader = None
        self.response_limit = response_limit

    def read_output(self):
        def read():
            try:
                while True:
                    line = self.child.stdout.readline(self.response_limit + 1)
                    if not line:
                        return
                    require(len(line) <= self.response_limit and line.endswith(b"\n"), "unbounded protocol output")
                    self.messages.put(json.loads(line), timeout=3)
            except BaseException as error:
                self.messages.put(error, timeout=3)
        self.reader = threading.Thread(target=read, daemon=True)
        self.reader.start()

    def send(self, value):
        self.child.stdin.write(json.dumps(value, separators=(",", ":")).encode() + b"\n")
        self.child.stdin.flush()

    def receive(self):
        value = self.messages.get(timeout=5)
        if isinstance(value, BaseException):
            raise value
        return value

    def initialize(self, *, resources=False):
        self.send({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
            "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "independent-wire", "version": "1"}}})
        response = self.receive()
        require(response["result"]["protocolVersion"] == "2025-11-25", "wrong legacy negotiation")
        require(response["result"]["capabilities"] == ({"tools": {}, "resources": {}} if resources else {"tools": {}}), "unexpected capabilities")
        self.send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def finish(self, expected=0):
        code = self.child.wait(timeout=6)
        if self.reader:
            self.reader.join(timeout=2)
            require(not self.reader.is_alive(), "stdout observer did not terminate")
        stderr = self.child.stderr.read(65537)
        require(len(stderr) < 65537, "unbounded stderr")
        require(code == expected if expected == 0 else code != 0, "unexpected process exit")
        if expected == 0:
            require(b"all admitted work released" in stderr, "missing joined cleanup receipt")
        require(b"DO_NOT_DISCLOSE" not in stderr, "private fixture content leaked")
        return stderr

    def close(self):
        if self.child.poll() is None:
            self.child.kill()
            self.child.wait(timeout=5)
        for stream in (self.child.stdin, self.child.stdout, self.child.stderr):
            stream.close()
        if self.reader:
            self.reader.join(timeout=2)
            require(not self.reader.is_alive(), "fixture reader leaked")

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()


def result(response, schema):
    value = response["result"]["structuredContent"]
    require(json.loads(response["result"]["content"][0]["text"]) == value, "text/structured disagreement")
    Draft202012Validator(schema).validate(value)
    require(value["profile"] == "haystack-h4-json-v1", "unexpected output profile")
    return value


def qualify(binary, directory):
    config = fixture(directory)
    with Session(binary, config) as session:
        session.read_output()
        session.initialize()
        session.send({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})
        tools = {tool["name"]: tool for tool in session.receive()["result"]["tools"]}
        require(set(tools) == {"haystack_read", "haystack_query"}, "wrong tool inventory")
        for tool in tools.values():
            Draft202012Validator.check_schema(tool["inputSchema"])
            Draft202012Validator.check_schema(tool["outputSchema"])
            require(tool["annotations"] == {"readOnlyHint": True, "destructiveHint": False,
                                          "idempotentHint": True, "openWorldHint": False}, "wrong tool annotations")
        for name, arguments in [("haystack_read", {"ids": ["fixture-a"]}), ("haystack_query", {"filter": "site"})]:
            Draft202012Validator(tools[name]["inputSchema"]).validate(arguments)
            session.send({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": name, "arguments": arguments}})
            page = result(session.receive(), tools[name]["outputSchema"])
            require(page["ok"] and page["complete"] and page["cursor"] is None and page["row_count"] == 1, "incorrect scoped page")
            require("Fixture Site" in json.dumps(page) and "DO_NOT_DISCLOSE" not in json.dumps(page)
                    and "Hidden" not in json.dumps(page), "scope or projection failed")
        invalid = {"ids": ["fixture-a"], "principal": "admin"}
        require(not Draft202012Validator(tools["haystack_read"]["inputSchema"]).is_valid(invalid), "schema permits authority injection")
        session.send({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "haystack_read", "arguments": invalid}})
        require(session.receive()["error"]["code"] == -32602, "runtime permits authority injection")
        session.child.stdin.close()
        session.finish()
    print("PASS installed discovery, independent input/output schemas, useful reads, scope, EOF cleanup", flush=True)
    for signum in (signal.SIGTERM, signal.SIGINT):
        with Session(binary, config) as session:
            session.read_output()
            session.initialize()
            session.child.send_signal(signum)  # stdin deliberately stays open
            session.finish()
    print("PASS SIGTERM/SIGINT with stdin open and joined process cleanup", flush=True)
    with Session(binary, config) as session:
        session.child.stdout.close()
        session.send({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
            "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "broken", "version": "1"}}})
        session.finish(expected=1)
    config = fixture(directory, large=True, timeout=300)
    with Session(binary, config) as session:
        # Drain only initialization; the large tool result fills the actual OS
        # pipe with no observer. A process exit proves no blocking stdin thread.
        session.send({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
            "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "slow", "version": "1"}}})
        require(json.loads(session.child.stdout.readline()) ["id"] == 0, "initialization failed")
        session.send({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "haystack_read", "arguments": {"ids": ["fixture-a"]}}})
        session.child.wait(timeout=5)
        session.finish(expected=1)
    print("PASS broken output and OS pipe backpressure have finite failed shutdown", flush=True)
    config = fixture(directory, large=True, timeout=2000)
    for stop in ("eof", signal.SIGINT, signal.SIGTERM):
        with Session(binary, config) as session:
            session.send({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
                "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "blocked-stop", "version": "1"}}})
            require(json.loads(session.child.stdout.readline())["id"] == 0, "initialization failed")
            session.send({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "haystack_read", "arguments": {"ids": ["fixture-a"]}}})
            require(select.select([session.child.stdout], [], [], 2)[0], "response never reached the pipe")
            prefix = os.read(session.child.stdout.fileno(), 32)
            require(len(prefix) > 0 and b"\n" not in prefix, "expected a partial physical response")
            time.sleep(0.05)
            queued = array.array("i", [0])
            fcntl.ioctl(session.child.stdout.fileno(), termios.FIONREAD, queued, True)
            first = queued[0]
            time.sleep(0.1)
            fcntl.ioctl(session.child.stdout.fileno(), termios.FIONREAD, queued, True)
            require(0 < first == queued[0] < 100000 and session.child.poll() is None,
                    "large undrained response did not stabilize in the output pipe")
            if stop == "eof":
                session.child.stdin.close()
            else:
                session.child.send_signal(stop)
            session.finish()
    print("PASS EOF/SIGINT/SIGTERM during observed partial blocked output exit zero after joined cleanup", flush=True)
    for config_fifo in (True, False):
        fifo = directory / ("config-private.fifo" if config_fifo else "dataset-private.fifo")
        os.mkfifo(fifo, 0o600)
        if config_fifo:
            launch = fifo
        else:
            launch = directory / "fifo-dataset.toml"
            launch.write_text('version = 1\ndataset_file = "dataset-private.fifo"\ndataset_format = "trio"\n'
                              'principal = "fixture-reader"\nentity_ids = ["fixture-a"]\n')
        checked = subprocess.run([str(binary), "mcp", "stdio", "--config", str(launch)],
                                 capture_output=True, timeout=2)
        require(checked.returncode != 0 and not checked.stdout and b"must be regular files" in checked.stderr
                and b"private.fifo" not in checked.stderr, "FIFO launch did not fail promptly and safely")
    print("PASS configuration and dataset FIFOs without writers are rejected promptly", flush=True)
    bad = directory / "bad.toml"
    bad.write_text('version = 99\npassword = "DO_NOT_DISCLOSE"\n')
    checked = subprocess.run([str(binary), "mcp", "stdio", "--config", str(bad)], capture_output=True, timeout=5)
    require(checked.returncode != 0 and not checked.stdout and b"DO_NOT_DISCLOSE" not in checked.stderr, "configuration diagnostics leak")
    checked = subprocess.run([str(binary), "mcp", "stdio", "--config", str(config)], stdin=subprocess.DEVNULL, capture_output=True, timeout=5)
    require(checked.returncode != 0 and b"requires Unix pipe stdin" in checked.stderr, "non-pipe profile was accepted")
    print("PASS bounded configuration errors and non-pipe launch rejection", flush=True)


def install(directory):
    installation = directory / "install"
    subprocess.run(["cargo", "+1.99.1", "install", "--locked", "--path", "haystack-cli", "--features", "mcp", "--debug",
                    "--root", str(installation), "--target-dir", str(ROOT / "target")], cwd=ROOT, check=True, timeout=600)
    return installation / "bin" / "haystack"


def qualify_rich(binary, directory):
    import rich
    directory = directory / "rich-stdio"
    directory.mkdir()
    rich.write_dataset(directory)
    config = directory / "mcp.toml"
    config.write_text('version = 1\ndataset_file = "rich.json"\ndataset_format = "typed-payload-v1"\n'
                      f'output_profile = "{rich.PROFILE}"\nnominal_specs = ["sys::Version"]\n'
                      f'principal = "fixture-reader"\nentity_ids = {json.dumps(rich.IDS)}\nhidden_tags = ["secret"]\n')
    with Session(binary, config) as session:
        session.read_output()
        session.initialize()
        session.send({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})
        tools = {tool["name"]: tool for tool in session.receive()["result"]["tools"]}
        ident = 1
        def call(name, args):
            nonlocal ident
            ident += 1
            session.send({"jsonrpc": "2.0", "id": ident, "method": "tools/call", "params": {"name": name, "arguments": args}})
            return session.receive()
        rich.qualify(tools, call)
        session.child.stdin.close()
        session.finish()
    original = config.read_text()
    for nominal in ('"unknown::Scalar"', '"sys::Int"', '"sys::Version", "sys::Version"'):
        config.write_text(original.replace('"sys::Version"', nominal))
        with Session(binary, config) as session:
            session.child.stdin.close()
            session.finish(expected=1)
            require(not session.child.stdout.read(), "invalid nominal scope wrote protocol data")
    config.write_text(original.replace(rich.PROFILE, "haystack-h4-json-v1"))
    with Session(binary, config) as session:
        session.child.stdin.close()
        session.finish(expected=1)
    config.write_text(original.replace(f'output_profile = "{rich.PROFILE}"\nnominal_specs = ["sys::Version"]\n', ''))
    with Session(binary, config) as session:
        session.read_output()
        session.initialize()
        session.send({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})
        tools = {tool["name"]: tool for tool in session.receive()["result"]["tools"]}
        require("profile" not in tools["haystack_read"]["inputSchema"]["properties"], "typed input implicitly selected rich output")
        session.send({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "haystack_read", "arguments": {"ids": ["a"], "projection": ["id", "nominal"]}}})
        page = result(session.receive(), tools["haystack_read"]["outputSchema"])
        require(page["ok"] and "nominal" not in page["grid"]["rows"][0], "default H4 gained nominal visibility")
        session.send({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "haystack_read", "arguments": {"ids": ["a"]}}})
        page = result(session.receive(), tools["haystack_read"]["outputSchema"])
        require(not page["ok"] and page["error"]["code"] == "projection", "default H4 coerced rich typed input")
        session.child.stdin.close()
        session.finish()
    print("PASS installed CLI rejects unadmitted/duplicate/native nominal scope and H4 nominal authority", flush=True)


def qualify_discovery_floor(binary, directory):
    directory = directory / "discovery-floor-stdio"
    directory.mkdir()
    config = fixture(directory)
    base = config.read_text()
    for profile, limit in (("haystack-h4-json-v1", 8192), ("haystack-jeto-v1", 16384)):
        config.write_text(base + f'output_profile = "{profile}"\nmax_response_bytes = {limit}\n')
        with Session(binary, config, response_limit=limit) as session:
            session.read_output()
            session.initialize()
            ident = "\u0001" * 128
            session.send({"jsonrpc": "2.0", "id": ident, "method": "tools/list"})
            reply = session.receive()
            require(reply["id"] == ident and {tool["name"] for tool in reply["result"]["tools"]} == {"haystack_read", "haystack_query"}, "discovery floor lost request identity or tools")
            session.child.stdin.close()
            session.finish()
    for limit in (8192, 16383):
        config.write_text(base + f'output_profile = "haystack-jeto-v1"\nmax_response_bytes = {limit}\n')
        with Session(binary, config) as session:
            # Stdin remains open: failure must precede protocol serving.
            diagnostic = session.finish(expected=1)
            require(b"invalid MCP configuration" in diagnostic and not session.child.stdout.read(), "undersized rich launch was not rejected before serving")
    print("PASS installed stdio H4/Jeto discovery floors with maximally escaped request IDs and fail-fast rich launch rejection", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, help="Use an explicit existing binary; omit for a real temporary Cargo install")
    args = parser.parse_args()
    require(os.name == "posix", "selected qualification requires Unix pipes")
    with tempfile.TemporaryDirectory(prefix="haystack-mcp-") as scratch:
        directory = Path(scratch)
        binary = args.binary.resolve() if args.binary else install(directory)
        qualify(binary, directory)
        qualify_rich(binary, directory)
        qualify_discovery_floor(binary, directory)
        from history import qualify_stdio as qualify_history
        qualify_history(binary, directory)
        from semantics import qualify_stdio as qualify_semantics
        qualify_semantics(binary, directory)
    print("PASS task-owned processes, pipes and fixture data removed" + ("; temporary installation removed" if not args.binary else ""), flush=True)


if __name__ == "__main__":
    main()
