#!/usr/bin/env python3
"""Qualify the installed local MCP HTTP profile with an independent fixture issuer."""
import argparse
import array
import base64
import fcntl
import http.client
import json
import os
from pathlib import Path
import select
import signal
import socket
import subprocess
import tempfile
import termios
import time
from urllib.parse import urlsplit

from jsonschema import Draft202012Validator

ROOT = Path(__file__).resolve().parents[2]
VERSION = "2026-07-28"
ISSUER = "https://fixture-issuer.example.test"
SCOPE = "haystack:read"


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def compact(value):
    return json.dumps(value, separators=(",", ":")).encode()


def b64(value):
    return base64.urlsafe_b64encode(value).rstrip(b"=").decode()


class Issuer:
    def __init__(self, directory):
        self.private = directory / "fixture-private.pem"
        generated = subprocess.run(["openssl", "genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048", "-out", str(self.private)], capture_output=True, timeout=15)
        require(generated.returncode == 0, "fixture key generation failed")
        self.private.chmod(0o600)
        public = subprocess.run(["openssl", "rsa", "-in", str(self.private), "-RSAPublicKey_out", "-outform", "DER"], capture_output=True, timeout=5)
        require(public.returncode == 0, "fixture public key export failed")
        self.public = base64.b64encode(public.stdout).decode()

    def token(self, resource, *, claims=None, header=None, raw_claims=None):
        now = int(time.time())
        payload = {"iss": ISSUER, "aud": resource, "sub": "alice", "scope": SCOPE,
                   "iat": now - 1, "exp": now + 3600, "jti": "fixture", "client_id": "fixture-client"}
        payload.update(claims or {})
        protected = {"alg": "RS256", "typ": "at+jwt", "kid": "fixture"}
        protected.update(header or {})
        signing = (b64(compact(protected)) + "." + b64(raw_claims if raw_claims is not None else compact(payload))).encode()
        result = subprocess.run(["openssl", "dgst", "-sha256", "-sign", str(self.private)], input=signing, capture_output=True, timeout=5)
        require(result.returncode == 0, "fixture token signing failed")
        return signing.decode() + "." + b64(result.stdout)


def config_file(directory, issuer, *, large=False, timeout=2500):
    dataset = directory / "entities.trio"
    dataset.write_text('id: @a\nsite\ndis: "Fixture Site A"\nsecret: "DO_NOT_DISCLOSE"\ntarget: @hidden "Denied display"\n'
                       + ('large: ' + json.dumps('\u0001' * 150000) + '\n' if large else '')
                       + '\n---\nid: @b\nsite\ndis: "Fixture Site B"\n\n---\nid: @hidden\nsite\ndis: "Denied Entity"\n')
    config = directory / "http.toml"
    config.write_text('version = 1\ndataset_file = "entities.trio"\ndataset_format = "trio"\n'
                      'listen = "127.0.0.1:0"\n' + f'issuer = "{ISSUER}"\nrequired_scope = "{SCOPE}"\n'
                      f'request_timeout_ms = {timeout}\nmax_response_bytes = 16777216\n'
                      'revoked_jti = ["revoked"]\nallowed_origins = ["https://allowed.example.test:443"]\n'
                      '[[keys]]\nkid = "fixture"\nalgorithm = "RS256"\n'
                      f'public_key_der_base64 = "{issuer.public}"\n'
                      '[[subjects]]\nsubject = "alice"\nentity_ids = ["a", "b"]\nhidden_tags = ["secret"]\n'
                      '[[subjects]]\nsubject = "bob"\nentity_ids = ["b"]\nhidden_tags = ["secret"]\n')
    return config


class Endpoint:
    def __init__(self, binary, config, issuer, *, response_limit=1024 * 1024):
        self.issuer = issuer
        self.response_limit = response_limit
        self.child = subprocess.Popen([str(binary), "mcp", "http", "--config", str(config)], stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            require(select.select([self.child.stderr], [], [], 10)[0], "HTTP readiness timed out")
            ready = self.child.stderr.readline(4096)
            require(ready.startswith(b"MCP HTTP ready: ") and ready.endswith(b"\n"), "HTTP startup failed")
            self.resource = ready[len(b"MCP HTTP ready: "):].strip().decode()
            address = urlsplit(self.resource)
            require(address.scheme == "http" and address.hostname == "127.0.0.1" and address.path == "/mcp" and address.port, "incorrect protected resource")
            self.address = (address.hostname, address.port)
            self.token = issuer.token(self.resource)
        except BaseException:
            self.close()
            raise

    def request(self, value=None, *, method="POST", path="/mcp", token=None, replace=None, extra=(), raw=None):
        body = compact(value) if raw is None and value is not None else (raw or b"")
        headers = {"Host": f"{self.address[0]}:{self.address[1]}", "Authorization": "Bearer " + (self.token if token is None else token),
                   "Content-Type": "application/json", "Accept": "application/json, text/event-stream", "MCP-Protocol-Version": VERSION,
                   "Mcp-Method": value.get("method", "ping") if isinstance(value, dict) else "ping", "Content-Length": str(len(body))}
        if isinstance(value, dict) and isinstance(value.get("params"), dict):
            key = "uri" if value.get("method") == "resources/read" else "name"
            if key in value["params"]:
                headers["Mcp-Name"] = value["params"][key]
        for key, val in (replace or {}).items():
            headers.pop(key, None)
            if val is not None:
                headers[key] = val
        connection = http.client.HTTPConnection(*self.address, timeout=4)
        try:
            connection.putrequest(method, path, skip_host=True, skip_accept_encoding=True)
            for key, val in list(headers.items()) + list(extra):
                connection.putheader(key, val)
            connection.endheaders(body)
            response = connection.getresponse()
            output = response.read(self.response_limit + 1)
            require(len(output) <= self.response_limit, "unbounded HTTP response")
            require(b"DO_NOT_DISCLOSE" not in output and b"Denied Entity" not in output and b"Denied display" not in output, "private entity content disclosed")
            return response.status, dict(response.getheaders()), json.loads(output) if output else None
        finally:
            connection.close()

    def stop(self, signum=signal.SIGTERM):
        self.child.send_signal(signum)
        stdout, stderr = self.child.communicate(timeout=6)
        require(self.child.returncode == 0, "HTTP child failed during owned shutdown")
        require(not stdout and len(stderr) < 65536 and b"all admitted work released" in stderr, "missing owned cleanup receipt or impure stdout")
        require(b"DO_NOT_DISCLOSE" not in stderr and self.token.encode() not in stderr, "diagnostics disclose protected data")
        with socket.socket() as rebound:
            rebound.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            rebound.bind(self.address)

    def close(self):
        if self.child.poll() is None:
            self.child.kill()
            self.child.wait(timeout=5)
        for stream in (self.child.stdout, self.child.stderr):
            stream.close()

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()


def request(method, params=None, ident=1):
    params = dict(params or {})
    params["_meta"] = {"io.modelcontextprotocol/protocolVersion": VERSION, "io.modelcontextprotocol/clientCapabilities": {}}
    return {"jsonrpc": "2.0", "id": ident, "method": method, "params": params}


def tool_result(response, schema):
    status, _, response = response
    require(status == 200, "tool HTTP failure")
    content = response["result"]["structuredContent"]
    require(json.loads(response["result"]["content"][0]["text"]) == content, "text/structured disagreement")
    Draft202012Validator(schema).validate(content)
    return content


def qualify(binary, directory, issuer):
    config = config_file(directory, issuer)
    with Endpoint(binary, config, issuer) as endpoint:
        discovery = endpoint.request(request("server/discover"))
        require(discovery[0] == 200 and discovery[2]["result"]["supportedVersions"] == [VERSION], "modern discovery failed")
        require("mcp-session-id" not in {key.lower() for key in discovery[1]}, "unexpected legacy session")
        status, _, inventory = endpoint.request(request("tools/list"))
        require(status == 200, "tool listing failed")
        tools = {tool["name"]: tool for tool in inventory["result"]["tools"]}
        require(set(tools) == {"haystack_read", "haystack_query"}, "wrong tools")
        for tool in tools.values():
            Draft202012Validator.check_schema(tool["inputSchema"])
            Draft202012Validator.check_schema(tool["outputSchema"])
            require(tool["annotations"] == {"readOnlyHint": True, "destructiveHint": False, "idempotentHint": True, "openWorldHint": False}, "wrong tool annotation")
        for name, args in [("haystack_read", {"ids": ["a"]}), ("haystack_query", {"filter": "site"})]:
            Draft202012Validator(tools[name]["inputSchema"]).validate(args)
            page = tool_result(endpoint.request(request("tools/call", {"name": name, "arguments": args})), tools[name]["outputSchema"])
            require(page["ok"] and page["complete"] and page["row_count"] == (1 if name == "haystack_read" else 2), "wrong scoped result")
        first = tool_result(endpoint.request(request("tools/call", {"name": "haystack_query", "arguments": {"filter": "site", "page_size": 1}})), tools["haystack_query"]["outputSchema"])
        next_page = request("tools/call", {"name": "haystack_query", "arguments": {"filter": "site", "page_size": 1, "cursor": first["cursor"]}})
        crossed = tool_result(endpoint.request(next_page, token=issuer.token(endpoint.resource, claims={"sub": "bob"})), tools["haystack_query"]["outputSchema"])
        require(crossed["error"]["code"] == "stale_cursor", "cross-caller cursor accepted")
        require(tool_result(endpoint.request(next_page), tools["haystack_query"]["outputSchema"])["complete"], "same-caller continuation failed")
        invalid = {"ids": ["a"], "principal": "admin"}
        require(not Draft202012Validator(tools["haystack_read"]["inputSchema"]).is_valid(invalid), "schema allows authority injection")
        require(endpoint.request(request("tools/call", {"name": "haystack_read", "arguments": invalid}))[2]["error"]["code"] == -32602, "runtime allows authority injection")
        print("PASS installed modern discovery, independent schemas, useful scoped H4 reads and caller-bound cursors", flush=True)
        status, headers, _ = endpoint.request(request("tools/list"), replace={"Authorization": None})
        challenge = headers.get("www-authenticate", "")
        require(status == 401 and "resource_metadata=" in challenge and "error=" not in challenge, "missing Bearer challenge")
        status, _, metadata = endpoint.request(method="GET", path="/.well-known/oauth-protected-resource/mcp", replace={"Authorization": None})
        require(status == 200 and metadata["resource"] == endpoint.resource and metadata["authorization_servers"] == [ISSUER], "untrusted resource metadata")
        for change, status in [({"aud": "https://other.example"}, 401), ({"iss": "https://other.example"}, 401), ({"exp": 1}, 401), ({"nbf": 2**63}, 401), ({"jti": "revoked"}, 401), ({"scope": "other"}, 403), ({"sub": "unknown"}, 403)]:
            reply = endpoint.request(request("tools/list"), token=issuer.token(endpoint.resource, claims=change))
            require(reply[0] == status and reply[2] is None, "credential matrix mismatch")
            require(("invalid_token" if status == 401 else "insufficient_scope") in reply[1].get("www-authenticate", ""), "incorrect authentication error")
        for scope in ("haystack:read \n", "haystack:read bad\0scope", 'haystack:read bad"scope', "haystack:read bad\\scope", "haystack:read café", " haystack:read", "haystack:read ", "haystack:read  other", "", None, 7):
            reply = endpoint.request(request("tools/list"), token=issuer.token(endpoint.resource, claims={"scope": scope}))
            require(reply[0] == 401 and "invalid_token" in reply[1].get("www-authenticate", ""), "malformed signed scope accepted")
        require(endpoint.request(request("tools/list"), token=issuer.token(endpoint.resource, claims={"scope": "other haystack:read another"}))[0] == 200, "valid multiple scopes rejected")
        for header in ({"alg": "none"}, {"alg": "HS256"}, {"kid": "unknown"}, {"typ": "JWT"}, {"jku": "https://untrusted.invalid"}, {"crit": ["other"]}):
            require(endpoint.request(request("tools/list"), token=issuer.token(endpoint.resource, header=header))[0] == 401, "invalid protected header accepted")
        bad_signature = endpoint.token[:-10] + ("A" if endpoint.token[-10] != "A" else "B") + endpoint.token[-9:]
        require(endpoint.request(request("tools/list"), token=bad_signature)[0] == 401, "invalid signature accepted")
        require(endpoint.request(request("tools/list"), token=issuer.token(endpoint.resource, claims={"aud": ["other", endpoint.resource]}))[0] == 200, "valid audience array rejected")
        print("PASS independent ephemeral RS256 issuer, protected metadata, signature/claim/algorithm/revocation/scope rejection", flush=True)
        for replace, extra, status in [({"Host": "evil.example"}, (), 403), ({}, (("Forwarded", "host=evil.example"),), 403), ({}, (("Origin", "null"),), 403), ({}, (("Origin", "https://evil.example"),), 403), ({}, (("Origin", "https://allowed.example.test:443"),), 200), ({}, (("Authorization", "Bearer other"),), 400), ({}, (("MCP-Protocol-Version", VERSION),), 400), ({"Mcp-Method": "ping"}, (), 400), ({"Accept": "xapplication/json, text/event-stream"}, (), 406), ({"Content-Type": "text/plain"}, (), 415)]:
            require(endpoint.request(request("tools/list"), replace=replace, extra=extra)[0] == status, "authority/header matrix mismatch")
        for replace, extra in [({"Accept": "application/json"}, (("Accept", "text/event-stream"),)), ({"Accept": "APPLICATION/JSON, TEXT/EVENT-STREAM"}, ()), ({"Content-Type": "Application/JSON"}, ()), ({"Accept": "application/json;Q=1, text/event-stream;Q=0.5"}, ())]:
            require(endpoint.request(request("tools/list"), replace=replace, extra=extra)[0] == 200, "valid media negotiation lost at SDK boundary")
        for accept in ("application/json;Q=0, text/event-stream", "application/json;q=NaN, text/event-stream", "application/json;q=1.001, text/event-stream", "application/json;q=0.1234, text/event-stream", "application/json;garbage, text/event-stream"):
            require(endpoint.request(request("tools/list"), replace={"Accept": accept})[0] == 406, "malformed or unacceptable media negotiation admitted")
        require(endpoint.request(request("tools/list"), method="GET")[0] == 405, "GET stream accepted")
        require(endpoint.request(request("tools/list"), path="/mcp?access_token=other")[0] == 400, "query token accepted")
        require(endpoint.request(request("initialize"))[0] == 404, "legacy initialization accepted")
        require(endpoint.request(raw=b"x" * 65537)[0] == 413, "body bound missing")
        require(endpoint.request(raw=b"[" * 33 + b"0" + b"]" * 33)[0] == 400, "depth bound missing")
        for value in ([], {"jsonrpc": "2.0", "id": 1, "result": {}}, {"jsonrpc": "2.0", "id": None, "method": "ping"}):
            require(endpoint.request(value)[0] == 400, "invalid envelope accepted")
        print("PASS installed Host/Origin/proxy policy, routing headers, envelope kinds and input bounds", flush=True)
        endpoint.stop()
    for signum in (signal.SIGINT, signal.SIGTERM):
        config = config_file(directory, issuer, large=True)
        with Endpoint(binary, config, issuer) as endpoint, socket.socket() as peer:
            peer.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 1024)
            peer.settimeout(3)
            peer.connect(endpoint.address)
            body = compact(request("tools/call", {"name": "haystack_read", "arguments": {"ids": ["a"]}}))
            head = (f"POST /mcp HTTP/1.1\r\nHost: {endpoint.address[0]}:{endpoint.address[1]}\r\nAuthorization: Bearer {endpoint.token}\r\n"
                    f"Accept: application/json, text/event-stream\r\nContent-Type: application/json\r\nMCP-Protocol-Version: {VERSION}\r\n"
                    f"Mcp-Method: tools/call\r\nMcp-Name: haystack_read\r\nContent-Length: {len(body)}\r\n\r\n").encode()
            peer.sendall(head + body)
            prefix = bytearray()
            while not prefix.endswith(b"\r\n\r\n") and len(prefix) < 4096:
                chunk = peer.recv(1)
                require(chunk, "response ended before its headers")
                prefix.extend(chunk)
            require(prefix.startswith(b"HTTP/1.1 200"), "response did not begin")
            response_headers = dict(line.split(": ", 1) for line in prefix.decode().split("\r\n")[1:] if ": " in line)
            advertised = int(response_headers.get("content-length", "0"))
            require(advertised > 1000000, f"large response fixture failed: {advertised} bytes")
            queued = array.array("i", [0])
            prior, stable = -1, 0
            for _ in range(12):
                time.sleep(0.1)
                fcntl.ioctl(peer.fileno(), termios.FIONREAD, queued, True)
                stable = stable + 1 if queued[0] == prior else 0
                prior = queued[0]
                if stable >= 3:
                    break
            require(stable >= 3 and 0 < queued[0] < advertised and endpoint.child.poll() is None,
                    f"undrained output did not stabilize: queued={queued[0]}, stable={stable}")
            endpoint.stop(signum)
            remaining = bytearray()
            while True:
                data = peer.recv(8192)
                if not data:
                    break
                remaining.extend(data)
            require(len(remaining) < 1900000, "blocked response was not suppressed")
    print("PASS SIGINT/SIGTERM during observed blocked socket output, joined process exit, peer EOF and listener reuse", flush=True)
    bad = directory / "bad.toml"
    bad.write_text('version = 99\ncredential = "DO_NOT_DISCLOSE"\n')
    result = subprocess.run([str(binary), "mcp", "http", "--config", str(bad)], capture_output=True, timeout=5)
    require(result.returncode != 0 and not result.stdout and b"DO_NOT_DISCLOSE" not in result.stderr, "configuration diagnostics leak")
    fifo = directory / "private-config.fifo"
    os.mkfifo(fifo, 0o600)
    result = subprocess.run([str(binary), "mcp", "http", "--config", str(fifo)], capture_output=True, timeout=2)
    require(result.returncode != 0 and b"must be regular files" in result.stderr and b"private-config" not in result.stderr, "FIFO configuration blocked or leaked")
    print("PASS safe configuration failures and FIFO rejection", flush=True)


def install(directory, features):
    installation = directory / "install"
    subprocess.run(["cargo", "+1.99.1", "install", "--locked", "--path", "haystack-cli", "--features", features, "--debug", "--root", str(installation), "--target-dir", str(ROOT / "target")], cwd=ROOT, check=True, timeout=600)
    return installation / "bin" / "haystack"


def qualify_rich(binary, directory, issuer):
    import rich
    directory = directory / "rich-http"
    directory.mkdir()
    rich.write_dataset(directory)
    config = directory / "http.toml"
    config.write_text('version = 1\ndataset_file = "rich.json"\ndataset_format = "typed-payload-v1"\n'
                      f'output_profile = "{rich.PROFILE}"\nlisten = "127.0.0.1:0"\nissuer = "{ISSUER}"\nrequired_scope = "{SCOPE}"\n'
                      '[[keys]]\nkid = "fixture"\nalgorithm = "RS256"\n'
                      f'public_key_der_base64 = "{issuer.public}"\n[[subjects]]\nsubject = "alice"\n'
                      f'entity_ids = {json.dumps(rich.IDS)}\nhidden_tags = ["secret"]\nnominal_specs = ["sys::Version"]\n'
                      '[[subjects]]\nsubject = "bob"\n'
                      f'entity_ids = {json.dumps(rich.IDS)}\nhidden_tags = ["secret"]\n')
    with Endpoint(binary, config, issuer) as endpoint:
        status, _, inventory = endpoint.request(request("tools/list"))
        require(status == 200, "rich HTTP discovery failed")
        tools = {tool["name"]: tool for tool in inventory["result"]["tools"]}
        def call(name, args):
            status, _, response = endpoint.request(request("tools/call", {"name": name, "arguments": args}))
            require(status == 200, "rich HTTP call failed")
            return response
        rich.qualify(tools, call)
        bob = issuer.token(endpoint.resource, claims={"sub": "bob"})
        page = tool_result(endpoint.request(request("tools/call", {"name": "haystack_read", "arguments": {"ids": ["a"], "profile": rich.PROFILE}}), token=bob), tools["haystack_read"]["outputSchema"])
        require(page["ok"] and "nominal" not in page["grid"]["rows"][0], "HTTP nominal scope crossed subjects")
        args = {"filter": "site", "page_size": 1, "profile": rich.PROFILE}
        first = rich.page_result(call("haystack_query", args), tools["haystack_query"]["outputSchema"])
        crossed = tool_result(endpoint.request(request("tools/call", {"name": "haystack_query", "arguments": {**args, "cursor": first["cursor"]}}), token=bob), tools["haystack_query"]["outputSchema"])
        require(not crossed["ok"] and crossed["error"]["code"] == "stale_cursor", "rich cursor crossed authenticated callers")
        endpoint.stop()


def qualify_discovery_floor(binary, directory, issuer):
    directory = directory / "discovery-floor-http"
    directory.mkdir()
    config = config_file(directory, issuer)
    base = config.read_text()
    for profile, limit in (("haystack-h4-json-v1", 8192), ("haystack-jeto-v1", 16384)):
        config.write_text(f'output_profile = "{profile}"\n' + base.replace("max_response_bytes = 16777216", f"max_response_bytes = {limit}"))
        with Endpoint(binary, config, issuer, response_limit=limit) as endpoint:
            ident = "\u0001" * 128
            status, _, reply = endpoint.request(request("tools/list", ident=ident))
            require(status == 200 and reply["id"] == ident and {tool["name"] for tool in reply["result"]["tools"]} == {"haystack_read", "haystack_query"}, "HTTP discovery floor lost request identity or tools")
            endpoint.stop()
    for limit in (8192, 16383):
        config.write_text('output_profile = "haystack-jeto-v1"\n' + base.replace("max_response_bytes = 16777216", f"max_response_bytes = {limit}"))
        result = subprocess.run([str(binary), "mcp", "http", "--config", str(config)], capture_output=True, timeout=5)
        require(result.returncode != 0 and not result.stdout and b"invalid MCP HTTP configuration" in result.stderr
                and b"MCP HTTP ready:" not in result.stderr, "undersized rich HTTP launch was not rejected before readiness")
    print("PASS installed HTTP H4/Jeto discovery floors with maximally escaped request IDs and fail-fast rich launch rejection", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, help="Use an existing binary; this is not fresh-install evidence")
    parser.add_argument("--features", choices=("mcp-http", "mcp,mcp-http"), default="mcp-http")
    args = parser.parse_args()
    require(os.name == "posix", "installed qualification selects Unix signals/socket observers")
    with tempfile.TemporaryDirectory(prefix="haystack-mcp-http-") as scratch:
        directory = Path(scratch)
        binary = args.binary.resolve() if args.binary else install(directory, args.features)
        issuer = Issuer(directory)
        qualify(binary, directory, issuer)
        qualify_rich(binary, directory, issuer)
        qualify_discovery_floor(binary, directory, issuer)
        from history import qualify_http as qualify_history_http
        qualify_history_http(binary, directory, issuer)
        from semantics import qualify_http as qualify_semantics_http
        qualify_semantics_http(binary, directory, issuer)
        if args.features == "mcp,mcp-http":
            from qualify import qualify as qualify_stdio, qualify_rich as qualify_rich_stdio, qualify_discovery_floor as qualify_discovery_floor_stdio
            qualify_stdio(binary, directory)
            qualify_rich_stdio(binary, directory)
            qualify_discovery_floor_stdio(binary, directory)
            from history import qualify_stdio as qualify_history_stdio
            qualify_history_stdio(binary, directory)
            from semantics import qualify_stdio as qualify_semantics_stdio
            qualify_semantics_stdio(binary, directory)
    print("PASS fixture processes, sockets, keys and data removed" + ("; fresh temporary installation removed" if not args.binary else ""), flush=True)


if __name__ == "__main__":
    main()
