"""Independent schema and unedited generated-client proof against a real owner.

Run with the locked dev environment; all generated code and fixture output are
short-lived. No product Python package, internet server or credentials are used.
"""
from __future__ import annotations

from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, HTTPServer
import importlib
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import tempfile
import threading
import time
from typing import get_type_hints

import httpx
from jsonschema import Draft202012Validator
from openapi_python_client import generate
from openapi_python_client.config import Config, ConfigFile, MetaType
from openapi_spec_validator import validate as validate_openapi

ROOT = Path(__file__).resolve().parents[2]
HEADERS = {"Xeto-Version": "5", "Authorization": "BEARER authToken=schema-fixture"}
HTTP_TIMEOUT_SECONDS = 10.0


def generated_client(client_class, *, timeout_seconds: float = HTTP_TIMEOUT_SECONDS, **kwargs):
    # Shared by authenticated, rejected-auth and stalled-response requests.
    return client_class(timeout=httpx.Timeout(timeout_seconds), **kwargs)


def assert_stalled_cleanup(binary, client_class, read, body_class, version, accept):
    release = threading.Event()
    headers_sent = threading.Event()
    errors: list[Exception] = []
    process = None
    server = None
    worker = None
    # Independent of HTTPX: even the pre-fix unlimited client exits this probe.
    watchdog = None
    try:
        try:
            with fixture_process(binary) as (process, url):
                class StallBody(BaseHTTPRequestHandler):
                    def do_POST(self):
                        try:
                            body = self.rfile.read(int(self.headers["Content-Length"]))
                            upstream = httpx.post(url.removesuffix("/api") + self.path, content=body, headers=dict(self.headers), timeout=2.0)
                            assert upstream.status_code == 200
                            self.send_response(200)
                            self.send_header("Content-Type", "application/json")
                            self.send_header("Content-Length", str(len(upstream.content)))
                            self.end_headers()
                            self.wfile.flush()
                            headers_sent.set()
                            release.wait(5.0)
                        except Exception as error:
                            errors.append(error)
                    def log_message(self, *_):
                        pass
                server = HTTPServer(("127.0.0.1", 0), StallBody)
                server.timeout = 2.0
                worker = threading.Thread(target=server.handle_request, daemon=True)
                worker.start()
                watchdog = threading.Timer(5.0, release.set)
                started = time.monotonic()
                watchdog.start()
                with generated_client(client_class, base_url=f"http://127.0.0.1:{server.server_port}", headers=HEADERS, timeout_seconds=1.0) as client:
                    read.sync_detailed(client=client, body=body_class(id="asset"), xeto_version=version, accept=accept)
                raise AssertionError("stalled generated call unexpectedly completed")
        except httpx.ReadTimeout:
            assert headers_sent.is_set(), "request timed out before the real response body was stalled"
            assert time.monotonic() - started < 5.0, "outer watchdog, not client deadline, ended the stall"
            assert process is not None and process.poll() == 0, "timeout did not reach fixture cleanup"
            assert not errors, errors
            print("PASS: generated request read timeout reached clean owner/child shutdown before the independent watchdog")
    finally:
        release.set()
        if watchdog is not None:
            watchdog.cancel()
            watchdog.join(timeout=1)
        if worker is not None:
            worker.join(timeout=2)
            assert not worker.is_alive(), "stalled-response worker survived cleanup"
        if server is not None:
            server.server_close()
        assert watchdog is None or not watchdog.is_alive(), "watchdog survived cleanup"


def references_are_local(value: object, document: dict) -> None:
    if isinstance(value, dict):
        for key, child in value.items():
            if key == "$ref":
                assert isinstance(child, str) and child.startswith("#/"), child
                target = document
                for part in child[2:].split("/"):
                    target = target[part.replace("~1", "/").replace("~0", "~")]
            else:
                references_are_local(child, document)
    elif isinstance(value, list):
        for child in value:
            references_are_local(child, document)


@contextmanager
def fixture_process(binary: Path):
    process = subprocess.Popen(
        [str(binary)], cwd=ROOT, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, text=True,
    )
    try:
        ready: queue.Queue[str] = queue.Queue()
        reader = threading.Thread(target=lambda: ready.put(process.stdout.readline()), daemon=True)
        reader.start()
        line = ready.get(timeout=20)
        if not line:
            raise AssertionError("schema fixture failed before readiness")
        url = json.loads(line)["url"]
        yield process, url
    finally:
        try:
            process.communicate("\n", timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.communicate()
        reader.join(timeout=1)
        assert not reader.is_alive(), "fixture readiness reader survived cleanup"
        assert process.returncode == 0, "fixture owner did not terminate cleanly"


def run_fixture(binary: Path, temporary: Path) -> None:
    with fixture_process(binary) as (_, url):
        with httpx.Client(headers=HEADERS, timeout=10) as raw:
            first = raw.get(url + "/rusty-haystack/openapi.json")
            first.raise_for_status()
            assert first.headers["cache-control"] == "private, no-store"
            second = raw.get(url + "/rusty-haystack/openapi.json")
            assert first.content == second.content
            document = first.json()
            validate_openapi(document)
            # The standalone library is an actual exported artifact. Its refs
            # have their own root, so validate it separately from OpenAPI.
            standalone = document["x-rusty-haystack-json-schema"]
            references_are_local(standalone, standalone)
            without_library = {k: v for k, v in document.items() if k != "x-rusty-haystack-json-schema"}
            references_are_local(without_library, document)
            Draft202012Validator.check_schema(standalone)
            for schema in document["components"]["schemas"].values():
                Draft202012Validator.check_schema(schema)
            def type_schema(qname: str) -> dict:
                matches = [name for name, schema in standalone["$defs"].items() if schema.get("x-xeto-type") == qname]
                assert len(matches) == 1, (qname, matches)
                return {**standalone, "$ref": f"#/$defs/{matches[0]}"}
            asset_schema = type_schema("fixture::Asset")
            asset_validator = Draft202012Validator(asset_schema)
            asset_validator.validate({"id": "asset", "dis": "Alpha", "spec": "fixture::Asset"})
            assert not asset_validator.is_valid({"id": "asset", "dis": "Alpha", "label": None})
            assert not asset_validator.is_valid({"id": {"spec": "sys::Ref", "val": "asset"}, "dis": "Alpha"})
            assert not asset_validator.is_valid({"dis": "Alpha"})
            excluded_types = {item["declaration"] for item in document["x-rusty-haystack-profile"]["excluded"] if item["kind"] == "type"}
            unsupported = {"fixture::UnsupportedDigits", "fixture::UnsupportedDot", "fixture::UnsupportedNestedClass", "fixture::UnsupportedSpacedCount", "fixture::UnsupportedStackedCount", "fixture::UnsupportedBareBrace", "fixture::UnsupportedEscapedHyphen"}
            assert unsupported <= excluded_types
            assert not unsupported & {schema.get("x-xeto-type") for schema in document["components"]["schemas"].values()}
            container_validator = Draft202012Validator(type_schema("fixture::CodeContainer"))
            response = raw.post(url + "/sys.api::readById", json={"id":"codes"}, headers={"Accept":"application/json;box=none"})
            response.raise_for_status()
            value = response.json()
            assert value["item"] == {"spec":"fixture::RefinedCode", "code":"ABC"}
            assert value["items"] == [value["item"]]
            container_validator.validate(value)
            base = Draft202012Validator(type_schema("fixture::BaseCode"))
            assert base.is_valid({"spec":"fixture::RefinedCode", "code":"BCD"})
            for code in ["BCD", "Abc", "ABC\n", "ABC\r", "ABC\u2028", "ABC\u2029", ""]:
                invalid = {**value, "item": {**value["item"], "code": code}}
                # A base schema promises its inherited shape, not the derived
                # subtype's stronger A-prefix constraint.
                if code != "BCD":
                    assert not container_validator.is_valid(invalid), code
            refined = Draft202012Validator(type_schema("fixture::RefinedCode"))
            refined.validate({"code":"ABC"})
            for code in ["BCD", "Abc", "ABC\n", "ABC\r", "ABC\u2028", "ABC\u2029", ""]:
                assert not refined.is_valid({"code":code}), code
            counted = Draft202012Validator(type_schema("fixture::CountedCode"))
            for code in ["", "AB", "ABAB"]:
                counted.validate({"code":code})
            for code in ["A", "ABABAB", "\n", "AB\n", "AB\r", "AB\u2028", "AB\u2029"]:
                assert not counted.is_valid({"code":code}), code
            schema_file = temporary / "openapi.json"
            schema_file.write_bytes(first.content)
            config = Config.from_sources(
                ConfigFile(project_name_override="schema_client", package_name_override="schema_client", post_hooks=[]),
                MetaType.NONE, schema_file, "utf-8", False, temporary / "schema_client",
            )
            problems = generate(config=config)
            assert not problems, f"generator warnings/errors: {problems}"
            sys.path.insert(0, str(temporary))
            client_class = importlib.import_module("schema_client.client").Client
            unset = importlib.import_module("schema_client.types").UNSET
            read = importlib.import_module("schema_client.api.typed.sys_api_read_by_id")
            hints = get_type_hints(read.sync_detailed)
            body_class = hints["body"]
            version = list(hints["xeto_version"])[0]
            accept = list(hints["accept"])[0]
            # Every selected exported operation must have a generated endpoint.
            expected_ids = {path["post"]["operationId"] for path in document["paths"].values()}
            actual_modules = {path.stem for path in (temporary / "schema_client/api/typed").glob("*.py") if path.stem != "__init__"}
            # Generator snake_case is also checked via each endpoint's literal URL.
            assert len(actual_modules) == len(expected_ids), (actual_modules, expected_ids)
            for path in document["paths"]:
                assert any(f'"{path}"' in source.read_text() for source in (temporary / "schema_client/api/typed").glob("*.py")), path
            sent: list[dict] = []
            def capture(request: httpx.Request) -> None:
                assert request.headers["Accept"] == "application/json;box=none"
                assert request.headers["Xeto-Version"] == "5"
                sent.append(json.loads(request.content))
            client = generated_client(client_class, base_url=url.removesuffix("/api"), headers={**HEADERS, "Accept": "application/json;box=none"}, httpx_args={"event_hooks": {"request": [capture]}})
            operation = document["paths"]["/api/sys.api::readById"]["post"]
            request_schema = {"components": document["components"], **operation["requestBody"]["content"]["application/json"]["schema"]}
            def call(body: object, expected_status: int, expected_body: dict, *, valid_request: bool = True) -> tuple[object, object]:
                response = read.sync_detailed(client=client, body=body, xeto_version=version, accept=accept)
                assert response.status_code == expected_status, response.content
                assert sent[-1] == expected_body, sent[-1]
                validator = Draft202012Validator(request_schema)
                if valid_request:
                    validator.validate(sent[-1])
                else:
                    assert not validator.is_valid(sent[-1])
                value = json.loads(response.content)
                response_schema = {"components": document["components"], **operation["responses"][str(expected_status)]["content"]["application/json"]["schema"]}
                Draft202012Validator(response_schema).validate(value)
                if value is not None:
                    assert response.parsed is not None and hasattr(response.parsed, "to_dict"), type(response.parsed)
                    assert response.parsed.to_dict() == value
                return response.parsed, value
            with client:
                _, value = call(body_class(id="asset", checked=unset), 200, {"id": "asset"})
                asset_validator.validate(value)
                _, value = call(body_class(id="codes", checked=unset), 200, {"id":"codes"})
                container_validator.validate(value)
                call(body_class(id="asset", checked=False), 200, {"id": "asset", "checked": False})
                call(body_class(id="asset", checked=None), 200, {"id": "asset", "checked": None})
                _, value = call(body_class(id="missing", checked=unset), 404, {"id": "missing"})
                assert value["spec"] == "sys.api::UnknownEntityErr"
                call(body_class(id="missing", checked=False), 200, {"id": "missing", "checked": False})
                call(body_class(id=unset, checked=False), 200, {"checked": False})
                call(body_class(id=None, checked=False), 200, {"id": None, "checked": False})
                call(body_class(id=None, checked=unset), 404, {"id": None})
                _, value = call(body_class(id="dynamic", checked=unset), 406, {"id": "dynamic"})
                assert value["spec"] == "sys.api::NotAcceptableErr"
                _, value = call(body_class(id="asset", checked="invalid"), 400, {"id":"asset", "checked":"invalid"}, valid_request=False)
                assert value["spec"] == "sys.api::InvalidArgsErr"
                for operation_name in ["ops", "about"]:
                    endpoint = importlib.import_module(f"schema_client.api.typed.sys_api_{operation_name}")
                    types = get_type_hints(endpoint.sync_detailed)
                    result = endpoint.sync_detailed(client=client, body=types["body"](), xeto_version=list(types["xeto_version"])[0], accept=list(types["accept"])[0])
                    assert result.status_code == 200, result.content
                    value = json.loads(result.content)
                    shape = document["paths"][f"/api/sys.api::{operation_name}"]["post"]["responses"]["200"]["content"]["application/json"]["schema"]
                    Draft202012Validator({"components":document["components"], **shape}).validate(value)
                    assert result.parsed is not None and result.parsed.to_dict() == value
            client = generated_client(client_class, base_url=url.removesuffix("/api"), headers={**HEADERS, "Authorization":"BEARER authToken=not-a-session", "Accept":"application/json;box=none"}, httpx_args={"event_hooks":{"request":[capture]}})
            with client:
                _, value = call(body_class(id="asset", checked=unset), 403, {"id":"asset"})
                assert value["spec"] == "sys.api::AuthErr"
            auto = raw.post(url + "/sys.api::readById", json={"id": "dynamic"}, headers={"Accept": "application/json"})
            auto.raise_for_status()
            assert auto.json()["id"] == {"spec": "sys::Ref", "val": "dynamic"}
            assert not Draft202012Validator(type_schema("sys::Ref")).is_valid(auto.json()["id"])
            # The original typed function inventory agrees with all represented
            # paths plus explicit visible function exclusions.
            ops = raw.post(url + "/sys.api::ops", json={})
            ops.raise_for_status()
            actual = {row["qname"] for row in ops.json()["rows"]}
            represented = set(document["x-rusty-haystack-profile"]["operations"])
            excluded = {item["declaration"] for item in document["x-rusty-haystack-profile"]["excluded"] if item["kind"] == "function"}
            assert actual == represented | excluded, (actual, represented, excluded)
            print(f"PASS: OpenAPI 3.1, standalone Draft2020-12, {len(expected_ids)} generated operations, 14 generated HTTP cases, native-fit nested/List subtype, contextual Asset and boxed-Ref counterexample; no generator warnings")

    assert_stalled_cleanup(binary, client_class, read, body_class, version, accept)

def main() -> None:
    subprocess.run(["cargo", "+1.99.1", "build", "--locked", "-p", "rusty-haystack-server", "--example", "schema_fixture"], cwd=ROOT, check=True)
    target = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
    if not target.is_absolute():
        target = ROOT / target
    binary = target / "debug/examples" / ("schema_fixture.exe" if os.name == "nt" else "schema_fixture")
    with tempfile.TemporaryDirectory(prefix="haystack-schema-client-") as temporary:
        run_fixture(binary, Path(temporary))
    assert not Path(temporary).exists(), "generated temporary directory survived cleanup"
    print("PASS: both owned fixture children exited; readiness/proxy/watchdog threads joined; generated temporary directory removed")


if __name__ == "__main__":
    main()
