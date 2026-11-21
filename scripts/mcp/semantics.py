"""Independent oracle for the selected H4 semantic-resource projection.

Uses JSON Schema and literal checked-in bundled-definition expectations, not
product bindings/codecs. This does not claim full Haystack ontology conformance.
All requests are raw; no SDK stale-on-error cache can mask a rejected identity.
"""
import base64
import copy
import json
import re
from jsonschema import Draft202012Validator

PROFILE = "haystack-h4-semantics-v1"
FLOOR = 128 * 1024
LIBRARIES = ["ph", "phIoT"]
DEFINITIONS = ["active", "ahu", "airHandlingEquip", "equip", "lib:ph", "lib:phIoT", "point", "sensor", "site", "siteMeter"]
# Independent literal expectations read from haystack-core/data/defs.trio.
DOCS = {"active": "Working, operative, effective",
        "lib:ph": "Project Haystack core definitions",
        "lib:phIoT": "Project Haystack definitions for Internet of Things",
        "siteMeter": "Main meter for the associated site"}
URI = re.compile(r"haystack://(?P<dataset>[0-9a-f]{32})/(?P<incarnation>[0-9a-f]{32})/(?P<catalog>0|[1-9][0-9]*)/haystack-h4-semantics-v1/(?P<kind>lib|def)/(?P<library>[A-Za-z0-9_-]+)/(?P<name>[A-Za-z0-9_-]+)/(?P<binding>[A-Za-z0-9_-]{43})\Z")
SCHEMA = {
    "type": "object", "additionalProperties": False,
    "required": ["profile", "uri", "kind", "name", "library", "observation", "provenance", "grid"],
    "properties": {
        "profile": {"const": PROFILE}, "uri": {"type": "string", "maxLength": 1024},
        "kind": {"enum": ["definition", "library"]},
        "name": {"type": "string", "minLength": 1, "maxLength": 256},
        "library": {"type": "string", "minLength": 1, "maxLength": 256},
        "observation": {"type": "object", "additionalProperties": False, "required": ["dataset", "incarnation", "catalog"],
                        "properties": {"dataset": {"type": "string", "pattern": "^[0-9a-f]{32}$"}, "incarnation": {"type": "string", "pattern": "^[0-9a-f]{32}$"}, "catalog": {"type": "integer", "minimum": 0}}},
        "provenance": {"const": {"source": "attached-h4-namespace", "projection": "def-lib-doc-or-library-version-v1"}},
        "grid": {"type": "object", "additionalProperties": False, "required": ["_kind", "meta", "cols", "rows"],
                 "properties": {"_kind": {"const": "grid"}, "meta": {"const": {"complete": True}},
                                "cols": {"type": "array", "minItems": 2, "maxItems": 3},
                                "rows": {"type": "array", "minItems": 1, "maxItems": 1}}},
    },
}
VALIDATOR = Draft202012Validator(SCHEMA)


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def decode_key(text):
    value = base64.urlsafe_b64decode(text + "=" * (-len(text) % 4))
    require(base64.urlsafe_b64encode(value).rstrip(b"=").decode() == text, "noncanonical resource key")
    return value.decode("ascii")


def validate(result, descriptor):
    require(isinstance(result.get("contents"), list) and len(result["contents"]) == 1, "resource read must have one content")
    content = result["contents"][0]
    require(set(content) == {"uri", "mimeType", "text"}, "unexpected resource content fields")
    uri = descriptor["uri"]
    require(content["uri"] == uri and content["mimeType"] == "application/json", "resource identity/MIME mismatch")
    value = json.loads(content["text"])
    VALIDATOR.validate(value)
    require(value["uri"] == uri, "embedded resource URI mismatch")
    match = URI.fullmatch(uri)
    require(match is not None, "noncanonical selected URI")
    fields = match.groupdict()
    require(value["observation"] == {"dataset": fields["dataset"], "incarnation": fields["incarnation"], "catalog": int(fields["catalog"])}, "resource observation mismatch")
    kind = "library" if fields["kind"] == "lib" else "definition"
    name, library = decode_key(fields["name"]), decode_key(fields["library"])
    require(value["name"] == name and value["library"] == library and value["kind"] == kind, "URI/payload identity disagreement")
    require(descriptor["name"] == kind + ":" + name, "descriptor identity mismatch")
    require(library in LIBRARIES, "ungranted library disclosed")
    row = value["grid"]["rows"][0]
    columns = value["grid"]["cols"]
    if kind == "library":
        require(name == library and row == {"name": library, "version": "4.0.0"}, "library identity/version mismatch")
        require(columns == [{"name": "name"}, {"name": "version"}], "wrong library columns")
    else:
        require(name in DEFINITIONS and set(row) == {"def", "lib", "doc"}, "ungranted definition or raw metadata")
        require(row["def"] == {"_kind": "symbol", "val": name}, "lost H4 definition Symbol")
        expected_library = "ph" if name == "lib:ph" else "phIoT"
        require(library == expected_library and row["lib"] == {"_kind": "symbol", "val": expected_library}, "lost H4 library Symbol or wrong declaration owner")
        require(isinstance(row["doc"], str) and row["doc"], "missing definition documentation")
        if name in DOCS:
            require(row["doc"] == DOCS[name], "incorrect independently known documentation")
        require(columns == [{"name": "def"}, {"name": "doc"}, {"name": "lib"}], "wrong definition columns")
    return value


def controls(good, descriptor):
    value = json.loads(good["contents"][0]["text"])
    mutations = []
    for field in ["dataset", "incarnation", "catalog"]:
        bad = copy.deepcopy(value)
        bad["observation"][field] = 999 if field == "catalog" else "0" * 32
        mutations.append(bad)
    for field, bad_value in [("profile", "haystack-jeto-v1"), ("name", "unknown"), ("library", "sys"),
                             ("uri", value["uri"][:-1] + ("A" if value["uri"][-1] != "A" else "B"))]:
        bad = copy.deepcopy(value); bad[field] = bad_value; mutations.append(bad)
    for change in ["missing", "duplicate", "string-symbol", "wrong-name", "raw-tags", "doc", "provenance"]:
        bad = copy.deepcopy(value)
        if change == "missing": bad["grid"]["rows"] = []
        elif change == "duplicate": bad["grid"]["rows"].append(copy.deepcopy(bad["grid"]["rows"][0]))
        elif change == "string-symbol": bad["grid"]["rows"][0]["def"] = bad["name"]
        elif change == "wrong-name": bad["grid"]["rows"][0]["def"]["val"] = "site"
        elif change == "raw-tags": bad["grid"]["rows"][0]["is"] = ["marker"]
        elif change == "doc": bad["grid"]["rows"][0]["doc"] = "incorrect documentation"
        else: bad["provenance"]["source"] = "complete-upstream-conformance"
        mutations.append(bad)
    for bad in mutations:
        changed = copy.deepcopy(good); changed["contents"][0]["text"] = json.dumps(bad)
        try:
            validate(changed, descriptor)
        except Exception:
            pass
        else:
            raise RuntimeError("independent semantic oracle accepted a rejecting mutation")
    print(f"PASS independent resource oracle rejects {len(mutations)} identity/content/provenance mutations", flush=True)


def qualify(call, *, modern, libraries_only=False):
    resources = []
    cursor = None
    while True:
        reply = call("resources/list", {"cursor": cursor} if cursor is not None else {})
        require("result" in reply, "resource discovery failed")
        result = reply["result"]
        if modern:
            require(result.get("resultType") == "complete" and result.get("ttlMs") == 0 and result.get("cacheScope") == "private", "missing private zero-TTL discovery hints")
        page = result["resources"]
        require(isinstance(page, list) and page, "selected resource discovery was empty")
        resources.extend(page)
        cursor = result.get("nextCursor")
        if cursor is None:
            break
        require(isinstance(cursor, str) and cursor and len(resources) <= 32, "unbounded/invalid resource continuation")
    expected = ({"library:phIoT"} if libraries_only else
                {"library:" + name for name in LIBRARIES} | {"definition:" + name for name in DEFINITIONS})
    require({r["name"] for r in resources} == expected and len(resources) == len(expected), "missing/duplicate/ungranted resources")
    require(len({r["uri"] for r in resources}) == len(resources), "duplicate resource URIs")
    active = None
    for descriptor in resources:
        result = call("resources/read", {"uri": descriptor["uri"]})["result"]
        validate(result, descriptor)
        if modern:
            require(result.get("ttlMs") == 0 and result.get("cacheScope") == "private" and result.get("resultType") == "complete", "missing private zero-TTL read hints")
        if descriptor["name"] == "definition:active":
            controls(result, descriptor); active = descriptor
    require(libraries_only or active is not None, "known exact resource was not read")
    templates = call("resources/templates/list", {})["result"]
    require(templates["resourceTemplates"] == [], "unselected templates exposed")
    if modern:
        require(templates.get("ttlMs") == 0 and templates.get("cacheScope") == "private", "missing template cache hints")
    for method in ["resources/subscribe", "completion/complete", "prompts/list"]:
        require(call(method, {"uri": resources[0]["uri"]})["error"]["code"] == -32601, "unselected capability accepted")
    for uri in ["file:///private/unknown", "https://example.invalid/private", resources[0]["uri"] + "?alias=1"]:
        error = call("resources/read", {"uri": uri})["error"]
        require(error == {"code": -32602, "message": "Resource unavailable or stale; rediscover"}, "resource rejection disclosed details or wrong protocol error")
    return active or resources[0]


def tool_coexistence(call, profile, *, history=False):
    tools = {tool["name"]: tool for tool in call("tools/list", {})["result"]["tools"]}
    require(set(tools) == ({"haystack_read", "haystack_query", "haystack_history"} if history else {"haystack_read", "haystack_query"}), "semantic selection changed tool inventory")
    arguments = {"ids": ["a"]}
    if profile == "haystack-jeto-v1": arguments["profile"] = profile
    result = call("tools/call", {"name": "haystack_read", "arguments": arguments})["result"]
    page = result["structuredContent"]
    Draft202012Validator(tools["haystack_read"]["outputSchema"]).validate(page)
    require(json.loads(result["content"][0]["text"]) == page and page["profile"] == profile and page["ok"] and page["row_count"] == 1, "coexisting entity tool failed")
    if profile == "haystack-jeto-v1":
        from rich import decoded_rows
        require(decoded_rows(page)[0]["id"] == ("Ref", "a"), "coexisting Jeto entity identity changed")
    else:
        require(page["grid"]["rows"][0]["id"] == {"_kind": "ref", "val": "a"}, "coexisting H4 entity identity changed")
    if history:
        arguments = {"id": "a", "start": "1970-01-01T00:00:00Z UTC", "end": "1970-01-01T00:00:02Z UTC", "max_rows": 10, "max_bytes": 2097152, "timeout_ms": 1000}
        if profile == "haystack-jeto-v1": arguments["profile"] = profile
        result = call("tools/call", {"name": "haystack_history", "arguments": arguments})["result"]
        page = result["structuredContent"]
        Draft202012Validator(tools["haystack_history"]["outputSchema"]).validate(page)
        require(page["ok"] and page["complete"] and page["row_count"] == 1 and page["profile"] == profile, "coexisting history tool failed")


def write_history(directory):
    (directory / "history.json").write_text(json.dumps({"version": 1, "value": {"kind": "grid", "meta": {},
        "cols": [{"name": name, "meta": {}} for name in ("id", "ts", "val")],
        "rows": [{"id": {"kind": "ref", "value": "a", "display": None},
                  "ts": {"kind": "dateTime", "seconds": "1", "nanos": 0, "offset": 0, "timezone": "UTC"},
                  "val": {"kind": "bool", "value": True}}]}}))


def qualify_stdio(binary, directory):
    from qualify import Session
    from qualify_http import request
    directory = directory / "semantic-stdio"; directory.mkdir()
    (directory / "entities.trio").write_text('id: @a\nsite\nhis\nkind: "Bool"\ntz: "UTC"\n')
    write_history(directory)
    config = directory / "mcp.toml"
    for profile in ["haystack-h4-json-v1", "haystack-jeto-v1"]:
        text = (f'version = 1\ndataset_file = "entities.trio"\ndataset_format = "trio"\noutput_profile = "{profile}"\n'
                f'semantic_resources = "{PROFILE}"\nmax_response_bytes = {FLOOR}\nprincipal = "fixture-reader"\nentity_ids = ["a"]\n'
                f'h4_libraries = {json.dumps(LIBRARIES)}\nh4_definitions = {json.dumps(DEFINITIONS)}\n')
        if profile == "haystack-jeto-v1": text += 'nominal_specs = ["sys::Version"]\n'
        config.write_text(text)
        for modern in [False, True]:
            with Session(binary, config, response_limit=FLOOR) as session:
                session.read_output()
                if modern:
                    session.send(request("server/discover")); info = session.receive()
                    require(info["result"]["capabilities"] == {"tools": {}, "resources": {}}, "wrong selected capabilities")
                else:
                    session.initialize(resources=True)
                def call(method, params):
                    ident = "\u0001" * 128
                    session.send(request(method, params, ident) if modern else {"jsonrpc": "2.0", "id": ident, "method": method, "params": params})
                    reply = session.receive(); require(reply["id"] == ident, "resource request ID lost"); return reply
                tool_coexistence(call, profile)
                active = qualify(call, modern=modern)
                session.child.stdin.close(); session.finish()
            # Restart creates a new service authority even for identical files.
            with Session(binary, config, response_limit=FLOOR) as session:
                session.read_output(); session.initialize(resources=True)
                session.send({"jsonrpc": "2.0", "id": 1, "method": "resources/read", "params": {"uri": active["uri"]}})
                require(session.receive()["error"]["code"] == -32602, "old service resource silently rebound")
                session.child.stdin.close(); session.finish()
        config.write_text(text.replace(f'max_response_bytes = {FLOOR}\n', 'max_response_bytes = 2097152\nhistory_file = "history.json"\nhistory_read = true\n'))
        with Session(binary, config, response_limit=2097152) as session:
            session.read_output(); session.initialize(resources=True)
            def coexisting(method, params):
                session.send({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}); return session.receive()
            tool_coexistence(coexisting, profile, history=True)
            require(coexisting("resources/list", {})["result"]["resources"], "history selection lost resources")
            session.child.stdin.close(); session.finish()
    print("PASS installed stdio H4 resources beside H4/Jeto tools, both protocol versions, min budget, exact content/schema and restart rejection", flush=True)


def qualify_http(binary, directory, issuer):
    from qualify_http import Endpoint, request, ISSUER, SCOPE
    directory = directory / "semantic-http"; directory.mkdir()
    (directory / "entities.trio").write_text('id: @a\nsite\nhis\nkind: "Bool"\ntz: "UTC"\n')
    write_history(directory)
    config = directory / "mcp.toml"
    for profile in ["haystack-h4-json-v1", "haystack-jeto-v1"]:
        config.write_text(f'version = 1\ndataset_file = "entities.trio"\ndataset_format = "trio"\noutput_profile = "{profile}"\n'
                          f'semantic_resources = "{PROFILE}"\nmax_response_bytes = {FLOOR}\nlisten = "127.0.0.1:0"\nissuer = "{ISSUER}"\nrequired_scope = "{SCOPE}"\n'
                          '[[keys]]\nkid = "fixture"\nalgorithm = "RS256"\n' + f'public_key_der_base64 = "{issuer.public}"\n'
                          '[[subjects]]\nsubject = "alice"\nentity_ids = ["a"]\n' + f'h4_libraries = {json.dumps(LIBRARIES)}\nh4_definitions = {json.dumps(DEFINITIONS)}\n'
                          + ('nominal_specs = ["sys::Version"]\n' if profile == "haystack-jeto-v1" else '')
                          + '[[subjects]]\nsubject = "bob"\nentity_ids = ["a"]\nh4_libraries = ["phIoT"]\n')
        with Endpoint(binary, config, issuer, response_limit=FLOOR) as endpoint:
            status, _, info = endpoint.request(request("server/discover"))
            require(status == 200 and info["result"]["capabilities"] == {"tools": {}, "resources": {}}, "wrong selected HTTP capabilities")
            def call(method, params, token=None):
                ident = "\u0001" * 128
                status, _, reply = endpoint.request(request(method, params, ident), token=token)
                require(status in (200, 404) and reply["id"] == ident, "resource HTTP envelope failed")
                return reply
            tool_coexistence(call, profile)
            active = qualify(call, modern=True)
            bob = issuer.token(endpoint.resource, claims={"sub": "bob"})
            qualify(lambda method, params: call(method, params, bob), modern=True, libraries_only=True)
            require(call("resources/read", {"uri": active["uri"]}, bob)["error"]["code"] == -32602, "resource replay crossed subjects")
            value = request("resources/read", {"uri": active["uri"]})
            for name in (None, "wrong", "=?base64?invalid!?="):
                status, _, rejected = endpoint.request(value, replace={"Mcp-Name": name})
                require(status == 400 and rejected["error"]["code"] == -32020, "wrong/missing resource URI header accepted")
            encoded = "=?base64?" + base64.b64encode(active["uri"].encode()).decode() + "?="
            status, _, accepted = endpoint.request(value, replace={"Mcp-Name": encoded})
            require(status == 200, "encoded resource URI header rejected"); validate(accepted["result"], active)
            endpoint.stop()
        selected = config.read_text().replace(f'max_response_bytes = {FLOOR}\n', 'max_response_bytes = 2097152\nhistory_file = "history.json"\n').replace('subject = "alice"\n', 'subject = "alice"\nhistory_read = true\n')
        config.write_text(selected)
        with Endpoint(binary, config, issuer, response_limit=2097152) as endpoint:
            def coexisting(method, params):
                status, _, result = endpoint.request(request(method, params)); require(status == 200, "coexisting history HTTP failure"); return result
            tool_coexistence(coexisting, profile, history=True)
            require(coexisting("resources/list", {})["result"]["resources"], "history HTTP selection lost resources")
            endpoint.stop()
    print("PASS installed HTTP H4 resources beside H4/Jeto tools, min budget, exact schema/content, narrow library grants and URI header/scope controls", flush=True)
