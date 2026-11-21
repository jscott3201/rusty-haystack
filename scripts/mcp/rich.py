"""Independent fixtures and decoder for the selected all-boxed MCP Jeto profile.

This bounded decoder validates the independently authored fixture's selected
types, not the complete Xeto catalog or every Jeto grammar. It never imports the
Rust encoder or treats returned provenance as permission to admit a new catalog.
"""
import base64
import copy
import json
import re
import struct

from jsonschema import Draft202012Validator

PROFILE = "haystack-jeto-v1"
REPOSITORY = "https://github.com/Project-Haystack/xeto"
REVISION = "873b922451d3ef4c0c9c08ef3daa542f352d69f3"
IDS = ["a", "b", "null-member", "symbol", "remove", "coord", "xstr", "bad-nominal", "nan-payload", "huge"]


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def write_dataset(directory):
    def ref(ident):
        return {"kind": "ref", "value": ident, "display": None}

    nominal = {"kind": "nominal", "spec": "sys::Version", "catalog": REPOSITORY,
               "revision": REVISION, "value": "1.2.3"}
    row = {"id": ref("a"), "site": {"kind": "marker"},
           "large": {"kind": "int", "value": "9007199254740993"},
           "min": {"kind": "int", "value": "-9223372036854775808"},
           "max": {"kind": "int", "value": "9223372036854775807"},
           "float": {"kind": "float", "bits": "8000000000000000"},
           "number": {"kind": "number", "bits": "4045000000000000", "unit": "kW"},
           "buf": {"kind": "buf", "base64": "AP/7"}, "nominal": nominal,
           "none": {"kind": "none"}, "na": {"kind": "na"},
           "list": {"kind": "list", "items": [{"kind": "null"}, {"kind": "none"}, {"kind": "na"}, {"kind": "marker"}]},
           "dict": {"kind": "dict", "tags": {"present": {"kind": "int", "value": "7"}}},
           "secret": {"kind": "symbol", "value": "DO_NOT_DISCLOSE"},
           "target": ref("hidden"),
           "nestedDenied": {"kind": "list", "items": [{"kind": "dict", "tags": {"ref": ref("hidden")}}]},
           "nestedNominal": {"kind": "list", "items": [{**nominal, "spec": "sys::MimeType", "value": "DO_NOT_DISCLOSE"}]},
           "foreign": {**nominal, "revision": "foreign"}}
    unsupported = [
        ("null-member", {"kind": "null"}), ("symbol", {"kind": "symbol", "value": "fixture"}),
        ("remove", {"kind": "remove"}),
        ("coord", {"kind": "coord", "lat": "0000000000000000", "lng": "0000000000000000"}),
        ("xstr", {"kind": "xstr", "name": "demo", "value": "fixture"}),
        ("bad-nominal", {**nominal, "value": "not a version"}),
        ("nan-payload", {"kind": "float", "bits": "7ff8000000000042"}),
        ("huge", {"kind": "buf", "base64": base64.b64encode(bytes(150000)).decode()}),
    ]
    rows = [row, {"id": ref("b"), "site": {"kind": "marker"}},
            {"id": ref("hidden"), "secret": {"kind": "str", "value": "DO_NOT_DISCLOSE"}}]
    rows.extend({"id": ref(ident), "value": value} for ident, value in unsupported)
    path = directory / "rich.json"
    path.write_text(json.dumps({"version": 1, "value": {"kind": "grid", "meta": {}, "cols": [], "rows": rows}}))
    return path


def decode(value, *, nullable=False):
    if value is None:
        require(nullable, "null dictionary member lost its presence")
        return ("Null",)
    if isinstance(value, list):
        return ("List", tuple(decode(item, nullable=True) for item in value))
    require(isinstance(value, dict), "unboxed data scalar")
    if "val" not in value:
        require(value.get("spec", "sys::Dict") == "sys::Dict", "unexpected structural spec")
        return ("Dict", {key: decode(item) for key, item in value.items() if key != "spec"})
    spec, text = value.get("spec"), value["val"]
    require(isinstance(text, str) and set(value) <= ({"spec", "val", "dis"} if spec == "sys::Ref" else {"spec", "val"}), "malformed scalar box")
    if spec == "sys::Int":
        require(re.fullmatch(r"0|-?[1-9][0-9]*", text) is not None, "noncanonical Int")
        integer = int(text)
        require(-(2**63) <= integer < 2**63, "Int outside signed i64")
        return ("Int", integer)
    if spec == "sys::Float":
        return ("Float", struct.pack(">d", float(text)).hex())
    if spec == "sys::Number":
        require(text == "42.0kW", "unexpected fixture Number")
        return ("Number", "4045000000000000", "kW")
    if spec == "sys::Buf":
        require(re.fullmatch(r"[A-Za-z0-9_-]*", text) is not None, "non-Jeto Buf alphabet")
        data = base64.b64decode(text + "=" * (-len(text) % 4), altchars=b"-_", validate=True)
        require(base64.urlsafe_b64encode(data).rstrip(b"=").decode() == text, "noncanonical Buf trailing bits")
        return ("Buf", data)
    if spec in {"sys::Marker", "sys::None", "sys::NA"}:
        require(text == {"sys::Marker": "✓", "sys::None": "∅", "sys::NA": "NA"}[spec], "wrong singleton spelling")
        return (spec.removeprefix("sys::"),)
    if spec == "sys::Version":
        require(text == "1.2.3", "unadmitted nominal text")
        return ("Nominal", spec, REPOSITORY, REVISION, text)
    if spec in {"sys::Ref", "sys::Str"}:
        require(spec != "sys::Ref" or text, "empty Ref")
        return (spec.removeprefix("sys::"), text)
    raise RuntimeError("fixture decoder does not admit this scalar spec")


def page_result(response, schema):
    result = response["result"]
    page = result["structuredContent"]
    require(json.loads(result["content"][0]["text"]) == page, "rich text/structured disagreement")
    Draft202012Validator(schema).validate(page)
    require(result.get("isError", False) == (not page["ok"]), "rich MCP error flag disagreement")
    require("DO_NOT_DISCLOSE" not in json.dumps(page), "rich scope disclosure")
    return page


def decoded_rows(page):
    require(page["profile"] == PROFILE and page["ok"], "wrong rich profile")
    origin = page["catalog"]
    require(origin["repository"] == REPOSITORY and origin["revision"] == REVISION
            and re.fullmatch(r"[0-9a-f]{64}", origin["selection"]), "unadmitted catalog provenance")
    grid = page["grid"]
    require(grid["spec"] == "sys::Grid", "wrong grid box")
    names = [col["name"] for col in grid["cols"]]
    require(len(names) == len(set(names)), "duplicate columns")
    require(page["row_count"] == len(grid["rows"]), "row count disagreement")
    rows = []
    for row in grid["rows"]:
        require(set(row) <= set(names) | {"spec"}, "row member missing from columns")
        rows.append(decode(row)[1])
    return rows


def verify_rows(rows, expected_ids):
    identities = [row.get("id") for row in rows]
    require(all(isinstance(ident, tuple) and len(ident) == 2 and ident[0] == "Ref" for ident in identities), "row identity is not a Ref")
    ids = [ident[1] for ident in identities]
    require(sorted(ids) == sorted(expected_ids), "authorized row identities differ from the exact expected set")
    require(ids.count("a") == 1, "the rich fixture row must be decoded exactly once")
    for decoded in rows:
        if decoded["id"] != ("Ref", "a"):
            continue
        expected = {"large": ("Int", 9007199254740993), "min": ("Int", -(2**63)), "max": ("Int", 2**63 - 1),
                    "float": ("Float", "8000000000000000"), "number": ("Number", "4045000000000000", "kW"),
                    "buf": ("Buf", bytes([0, 255, 251])), "nominal": ("Nominal", "sys::Version", REPOSITORY, REVISION, "1.2.3"),
                    "none": ("None",), "na": ("NA",), "site": ("Marker",),
                    "list": ("List", (("Null",), ("None",), ("NA",), ("Marker",))),
                    "dict": ("Dict", {"present": ("Int", 7)})}
        for key, value in expected.items():
            require(decoded.get(key) == value, "rich semantic mismatch: " + key)
        for name in ("missing", "secret", "target", "nestedDenied", "nestedNominal", "foreign"):
            require(name not in decoded, "missing/denied member disclosed")


def verify_page(page, expected_ids=("a",)):
    verify_rows(decoded_rows(page), expected_ids)


def verify_pagination(first, second):
    require(not first["complete"] and first["cursor"] and first["row_count"] == 1, "rich first page is not a single continued row")
    require(second["complete"] and second["cursor"] is None and second["row_count"] == 1, "rich final page is not a single complete row")
    require(second["catalog"] == first["catalog"], "rich continuation changed immutable selection")
    verify_rows(decoded_rows(first) + decoded_rows(second), ("a", "b"))


def oracle_regression():
    """Cheap controls for the client oracle; no server, install or product codec."""
    row = {
        "id": {"spec": "sys::Ref", "val": "a"},
        "large": {"spec": "sys::Int", "val": "9007199254740993"},
        "min": {"spec": "sys::Int", "val": "-9223372036854775808"},
        "max": {"spec": "sys::Int", "val": "9223372036854775807"},
        "float": {"spec": "sys::Float", "val": "-0.0"},
        "number": {"spec": "sys::Number", "val": "42.0kW"},
        "buf": {"spec": "sys::Buf", "val": "AP_7"},
        "nominal": {"spec": "sys::Version", "val": "1.2.3"},
        "none": {"spec": "sys::None", "val": "∅"},
        "na": {"spec": "sys::NA", "val": "NA"},
        "site": {"spec": "sys::Marker", "val": "✓"},
        "list": [None, {"spec": "sys::None", "val": "∅"},
                 {"spec": "sys::NA", "val": "NA"}, {"spec": "sys::Marker", "val": "✓"}],
        "dict": {"present": {"spec": "sys::Int", "val": "7"}},
    }

    def page(rows, *, complete=True):
        return {"profile": PROFILE, "ok": True,
                "catalog": {"repository": REPOSITORY, "revision": REVISION, "selection": "0" * 64},
                "grid": {"spec": "sys::Grid", "cols": [{"name": key} for key in row], "rows": rows},
                "complete": complete, "cursor": None if complete else "fixture-cursor", "row_count": len(rows)}

    def rejected(check, label):
        try:
            check()
        except RuntimeError:
            return
        raise RuntimeError("oracle accepted negative control: " + label)

    other = {"id": {"spec": "sys::Ref", "val": "b"}}
    foreign = {"id": {"spec": "sys::Ref", "val": "c"}}
    verify_page(page([row]))
    # This first control reproduces the reviewed false positive without a
    # schema violation, count mismatch, server, or alternative codec.
    rejected(lambda: verify_page(page([other])), "requested a returned only b")
    rejected(lambda: verify_page(page([])), "requested a omitted its rich row")
    verify_page(page([row, other]), ("a", "b"))
    rejected(lambda: verify_page(page([other, foreign]), ("a", "b")), "query omitted the rich row")
    rejected(lambda: verify_page(page([row, row]), ("a", "b")), "query duplicated a and omitted b")
    verify_pagination(page([row], complete=False), page([other]))
    verify_pagination(page([other], complete=False), page([row]))
    rejected(lambda: verify_pagination(page([row], complete=False), page([row])), "continuation duplicated a")
    rejected(lambda: verify_pagination(page([row], complete=False), page([])), "continuation omitted b")
    rejected(lambda: verify_pagination(page([other], complete=False), page([foreign])), "continuation omitted rich a")
    changed = copy.deepcopy(row)
    changed["large"]["val"] = "9007199254740992"
    rejected(lambda: verify_page(page([changed])), "rich Int semantic mismatch")
    print("PASS independent oracle rejects wrong, missing and duplicate rows and preserves exact rich semantics", flush=True)


def qualify(tools, call):
    oracle_regression()
    require(set(tools) == {"haystack_read", "haystack_query"}, "rich tool inventory changed")
    for tool in tools.values():
        for schema in (tool["inputSchema"], tool["outputSchema"]):
            Draft202012Validator.check_schema(schema)
            def local_refs(node):
                if isinstance(node, dict):
                    require("$ref" not in node or node["$ref"].startswith("#/"), "remote schema reference")
                    for child in node.values():
                        local_refs(child)
                elif isinstance(node, list):
                    for child in node:
                        local_refs(child)
            local_refs(schema)
        require(tool["inputSchema"]["properties"]["profile"] == {"type": "string", "const": PROFILE}, "profile is not a fixed acknowledgment")
    read = tools["haystack_read"]
    for args in ({"ids": ["a"]}, {"ids": ["a"], "profile": "haystack-h4-json-v1"},
                 {"ids": ["a"], "profile": None}, {"ids": ["a"], "profile": PROFILE, "boxing": "auto"},
                 {"ids": ["a"], "profile": PROFILE, "catalog": REPOSITORY}):
        require(not Draft202012Validator(read["inputSchema"]).is_valid(args), "rich input schema admitted profile/authority injection")
        require(call("haystack_read", args)["error"]["code"] == -32602, "rich runtime admitted profile/authority injection")
    for name, args in (("haystack_read", {"ids": ["a", "hidden", "missing"]}), ("haystack_query", {"filter": "site"})):
        args["profile"] = PROFILE
        Draft202012Validator(tools[name]["inputSchema"]).validate(args)
        page = page_result(call(name, args), tools[name]["outputSchema"])
        verify_page(page, ("a",) if name == "haystack_read" else ("a", "b"))
        require(page["complete"] and page["cursor"] is None and page["row_count"] == (1 if name == "haystack_read" else 2), "rich scoped count mismatch")
    schema = read["outputSchema"]
    original = page_result(call("haystack_read", {"ids": ["a"], "profile": PROFILE}), schema)
    verify_page(original)
    for bad in ({"spec": "sys::Bool"}, {"spec": "sys::Bool", "val": "maybe"},
                {"spec": "sys::Int", "val": 9007199254740993}, {"spec": "sys::Buf", "val": "AP/7="},
                {"spec": "sys::None", "val": "none"}, {"spec": "sys::Ref", "val": "a", "extra": "x"},
                {"nullMember": None}, 9007199254740993):
        malformed = copy.deepcopy(original)
        malformed["grid"]["rows"][0]["large"] = bad
        require(not Draft202012Validator(schema).is_valid(malformed), "output schema admitted malformed rich shape")
    for bad in ({"spec": "sys::Int", "val": str(2**63)}, {"spec": "sys::Buf", "val": "AB"}):
        try:
            decode(bad)
        except (RuntimeError, ValueError):
            pass
        else:
            raise RuntimeError("independent decoder missed semantic negative")
    args = {"filter": "site", "page_size": 1, "profile": PROFILE}
    first = page_result(call("haystack_query", args), tools["haystack_query"]["outputSchema"])
    second = page_result(call("haystack_query", {**args, "cursor": first["cursor"]}), tools["haystack_query"]["outputSchema"])
    verify_pagination(first, second)
    for ident in IDS[2:]:
        page = page_result(call("haystack_read", {"ids": [ident], "profile": PROFILE}), schema)
        require(not page["ok"] and "grid" not in page and page["error"]["code"] in ({"budget", "output_budget"} if ident == "huge" else {"projection"}), "unsupported rich value became a successful partial page: " + ident)
    print("PASS rich profile acknowledgment, independent boxed schemas/decoder, exact numeric identity, nominal provenance, Buf, absence, policy, cursors and whole-page errors", flush=True)


if __name__ == "__main__":
    oracle_regression()
