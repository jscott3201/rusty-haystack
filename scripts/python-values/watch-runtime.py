"""Installed-only real-server watch consumer with independent rejecting oracles."""
import argparse
import asyncio
import copy
import hashlib
import json
from pathlib import Path
import runpy
import sys

import rusty_haystack as rh
from rusty_haystack.client import AsyncWatch, WatchDelivery, WatchControl

helpers = runpy.run_path(str(Path(__file__).with_name("watch_fixture.py")))
Fixture = helpers["Fixture"]
IDENTITY = ("key", "authority", "watch_id", "dataset", "incarnation", "catalog_generation", "scope_generation", "token", "from_revision", "through", "initial")


def scalar(value):
    # Independently observe Python classes, exact bits and bytes, without
    # encoding through the production subscription/typed-v1 codecs again.
    if value is None:
        return ["null"]
    if isinstance(value, rh.NoneValue):
        return ["NoneValue"]
    if isinstance(value, rh.Int):
        return ["Int", value.value]
    if isinstance(value, rh.Float):
        return ["Float", value.bits]
    if isinstance(value, rh.Number):
        return ["Number", value.val, value.unit]
    if isinstance(value, rh.Buf):
        return ["Buf", list(value.data)]
    if isinstance(value, rh.Ref):
        return ["Ref", value.val]
    if isinstance(value, rh.Marker):
        return ["Marker"]
    if isinstance(value, rh.HDict):
        return ["HDict", {key: scalar(value[key]) for key in value.keys()}]
    if isinstance(value, rh.HList):
        return ["HList", [scalar(item) for item in value]]
    assert isinstance(value, str), type(value)
    return ["str", value]


def basic_row(ref, value):
    return {"id": ["Ref", ref], "point": ["Marker"], "value": ["Number", value, None]}


def rich_row(value, *, old):
    result = basic_row("a", value)
    result.update(exact=["Int", 2**63 - 1], bits=["Float", 0x7ff8000000000042], buf=["Buf", [0, 255]], unit=["Number", 21.5, "°C"], nothing=["null"], absent=["NoneValue"], nested=["HList", [["HDict", basic_row("nested", 7)]]])
    if old:
        result["old"] = ["str", "remove me"]
    return result


def snapshot(delivery):
    assert isinstance(delivery, WatchDelivery)
    result = {name: getattr(delivery, name) for name in IDENTITY}
    result["changed"] = sorted([scalar(row)[1] for row in delivery.changed], key=lambda row: row["id"][1])
    result["removed"] = list(delivery.removed)
    return result


def oracle(observed, expected_initial, expected_update):
    for name, expected in (("initial", expected_initial), ("update", expected_update)):
        actual = observed[name]
        for field in IDENTITY:
            assert actual[field] == expected[field], (name, field)
        for field in ("catalog_generation", "scope_generation", "from_revision", "through"):
            assert type(actual[field]) is int, (name, field, "integer fence")
        assert type(actual["initial"]) is bool
    initial, update = observed["initial"], observed["update"]
    assert initial["changed"] == [rich_row(1, old=True), basic_row("b", 2)]
    assert initial["removed"] == [] and initial["initial"] is True
    assert update["changed"] == [rich_row(15, old=False)]
    assert update["removed"] == ["b"] and update["initial"] is False
    assert update["from_revision"] == initial["through"] < update["through"]
    assert update["token"] != initial["token"]
    assert observed["replay"] == initial
    assert observed["extra_dispatch_after_rejection"] is False
    assert observed["ops"] == ["watchSub", "watchPoll", "watchAck", "watchAck", "watchPoll", "watchAck", "watchRenew", "watchUnsub"]
    assert observed["session_count"] == 1 and observed["exact_ack_retry"] is True
    assert observed["controls"] == {"ack_loss": ["unknown", None], "gap": ["resync", "gap"], "overflow": ["resync", "overflow"], "lease_expired": ["resync", "lease_expired"]}
    assert observed["child_active"] == 0 and observed["sibling_active"] == 1
    assert observed["parent_active"] == observed["parent_bindings"] == observed["parent_sockets"] == 0
    assert observed["key_ownership"] == {
        "pending_rejected": True, "acked_rejected": True, "unknown_retained": True,
        "capacity_rejected": True, "reservations": 32, "not_created_reused": True,
        "confirmed_reused": True, "sibling_survived": True, "parent_survived": True,
    }
    assert observed["cleanup_budget"] == [
        {"skipped": 6, "idle": False, "kind": "unknown", "client_create": 6, "client_unsub": 0, "committed": 0, "active": 0, "fixture_ops": []},
        {"skipped": 5, "idle": False, "kind": "closed", "client_create": 6, "client_unsub": 1, "committed": 1, "active": 0, "fixture_ops": []},
        {"skipped": 5, "idle": True, "kind": "closed", "client_create": 6, "client_unsub": 1, "committed": 1, "active": 0, "fixture_ops": ["watchAck", "watchSub"]},
    ]


def rejecting_controls(observed, expected_initial, expected_update):
    mutations = []
    def change(category, path, replacement):
        mutated = copy.deepcopy(observed)
        cursor = mutated
        for key in path[:-1]:
            cursor = cursor[key]
        cursor[path[-1]] = replacement
        mutations.append((category, mutated))
    change("native-values", ["initial", "changed", 0, "exact", 1], 2**63 - 2)
    change("native-values", ["initial", "changed", 0, "exact", 0], "Number")
    change("native-values", ["initial", "changed", 0, "bits", 1], 0x7ff8000000000043)
    change("native-values", ["initial", "changed", 0, "buf", 1], [0, 254])
    change("native-values", ["initial", "changed", 0, "unit", 2], "K")
    change("native-values", ["initial", "changed", 0, "nothing"], ["NoneValue"])
    change("native-values", ["initial", "changed", 0, "nested", 1, 0, 1, "value", 1], 8)
    change("authorized-complete-records", ["initial", "changed", 0, "id", 1], "denied")
    change("authorized-complete-records", ["initial", "changed"], [observed["initial"]["changed"][0]] * 2)
    old = copy.deepcopy(observed["update"]["changed"])
    old[0]["old"] = ["str", "remove me"]
    change("authorized-complete-records", ["update", "changed"], old)
    for field in ("token", "watch_id", "key", "authority"):
        change("watch-identity", ["initial", field], "changed")
    for field in ("dataset", "incarnation", "catalog_generation", "scope_generation", "from_revision", "through"):
        value = observed["update"][field]
        change("position-and-scope", ["update", field], value + 1 if isinstance(value, int) else "changed")
    change("position-and-scope", ["update", "through"], float(observed["update"]["through"]))
    for removed in ([], ["b", "b"], ["b", "denied"], ["a", "b"]):
        change("removal-set", ["update", "removed"], removed)
    change("phase-and-controls", ["initial", "initial"], False)
    for name in observed["controls"]:
        change("phase-and-controls", ["controls", name], ["stopped", None])
    change("request-sequence", ["extra_dispatch_after_rejection"], True)
    change("request-sequence", ["ops"], observed["ops"] + ["watchAck"])
    change("request-sequence", ["session_count"], 2)
    change("request-sequence", ["exact_ack_retry"], False)
    change("server-cleanup", ["child_active"], 1)
    change("server-cleanup", ["parent_bindings"], 1)
    change("watch-identity", ["key_ownership", "pending_rejected"], False)
    change("watch-identity", ["key_ownership", "unknown_retained"], False)
    change("request-sequence", ["key_ownership", "reservations"], 33)
    change("request-sequence", ["cleanup_budget", 0, "client_create"], 7)
    change("server-cleanup", ["cleanup_budget", 2, "kind"], "idle")
    for category, changed in mutations:
        try:
            oracle(changed, expected_initial, expected_update)
        except AssertionError:
            continue
        raise AssertionError(f"oracle accepted mutation: {category}")
    return {category: sum(name == category for name, _ in mutations) for category, _ in mutations}


async def terminal(binary, reason):
    with Fixture(binary) as f:
        async with rh.AsyncHaystackClient(f.url, "user", "pencil", timeout=3) as parent:
            watch = parent.watch(["a"], key=reason, lease_ms=150 if reason == "lease_expired" else 5000)
            first = await anext(watch)
            await watch.ack(first)
            if reason == "gap":
                for n in range(8):
                    f.command("update", id="parent", value=n)
            elif reason == "overflow":
                f.command("overflow")
            else:
                await f.wait(lambda s: s["active"] == 0)
            control = await anext(watch)
            assert isinstance(control, WatchControl)
            try:
                await anext(watch)
            except StopAsyncIteration:
                pass
            else:
                raise AssertionError("terminal stream continued")
            assert not any(x["op"] == "watchRenew" for x in f.command("stats")["log"])
            fresh = parent.watch(["parent"], key=reason + "-fresh")
            assert isinstance(await anext(fresh), WatchDelivery)
            return [control.kind, control.reason]


def rejects_without_dispatch(parent, fixture, ids, key, *, duplicate=True):
    before = fixture.command("stats")["log"]
    try:
        parent.watch(ids, key=key, lease_ms=5000)
    except rh.ClientError as error:
        if duplicate:
            assert "key" in str(error)
    else:
        raise AssertionError("a conflicting watch owner was admitted")
    assert fixture.command("stats")["log"] == before
    return True


async def ownership_controls(binary):
    with Fixture(binary) as f:
        async with rh.AsyncHaystackClient(f.url, "user", "pencil", timeout=3) as parent:
            owned = parent.watch(["a"], key="reserved", lease_ms=5000)
            first = await anext(owned)
            observed = {"pending_rejected": rejects_without_dispatch(parent, f, ["a"], "reserved")}
            await owned.ack(first)
            observed["acked_rejected"] = rejects_without_dispatch(parent, f, ["a"], "reserved")
            f.command("set", skip_unsub=100)
            report = await owned.aclose()
            assert report.kind == "unknown" and report.local_joined and owned.closed
            del owned
            observed["unknown_retained"] = rejects_without_dispatch(parent, f, ["a"], "reserved")
            assert f.command("stats")["active"] == 1
            f.command("set", skip_unsub=0)
            sibling = parent.watch(["parent"], key="distinct")
            sibling_initial = await anext(sibling)
            observed["sibling_survived"] = (await sibling.ack(sibling_initial)).kind == "acknowledged"
            assert (await sibling.aclose()).kind == "closed"
            reused = parent.watch(["parent"], key="distinct")
            observed["confirmed_reused"] = (await reused.ready()).kind == "closed"
            assert (await reused.aclose()).kind == "closed"
            children = []
            for index in range(31):
                children.append(parent.watch(["a"], key=f"unstarted-{index}"))
                if index % 8 == 7:
                    assert len(await parent.read('id==@parent')) == 1
            observed["reservations"] = len(children) + 1
            observed["capacity_rejected"] = rejects_without_dispatch(parent, f, ["a"], "over-capacity", duplicate=False)
            freed = children.pop()
            assert (await freed.aclose()).kind == "not_created"
            replacement = parent.watch(["a"], key=freed.key)
            observed["not_created_reused"] = (await replacement.aclose()).kind == "not_created"
            observed["unknown_retained"] &= rejects_without_dispatch(parent, f, ["a"], "reserved")
            observed["parent_survived"] = len(await parent.read('id==@parent')) == 1
            assert f.command("stats")["active"] == 1
        await f.wait(lambda stats: stats["active"] == 0 and stats["bindings"] == 0 and stats["ws_active"] == 0)
    return observed


async def cleanup_budget_controls(binary):
    observed = []
    for skipped, idle in ((6, False), (5, False), (5, True)):
        with Fixture(binary) as f:
            async with rh.AsyncHaystackClient(f.url, "user", "pencil", timeout=3) as parent:
                f.command("set", skip_create=skipped, idle_create=int(idle))
                watch = parent.watch(["b", "a", "a"], key="budget", lease_ms=5000)
                assert (await watch.ready()).kind == "unknown"
                report = await watch.aclose()
                assert report.local_joined and watch.closed
                stats = f.command("stats")
                calls = [entry for entry in stats["log"] if entry["key"] == "budget"]
                client_calls = [entry for entry in calls if not entry.get("fixture")]
                assert len({entry["request"] for entry in calls if entry["op"] == "watchSub"}) == 1
                assert len({entry["session"] for entry in calls}) == 1
                observed.append({"skipped": skipped, "idle": idle, "kind": report.kind,
                    "client_create": sum(entry["op"] == "watchSub" for entry in client_calls),
                    "client_unsub": sum(entry["op"] == "watchUnsub" for entry in client_calls),
                    "committed": stats["committed"], "active": stats["active"],
                    "fixture_ops": [entry["op"] for entry in calls if entry.get("fixture")]})
            await f.wait(lambda stats: stats["active"] == 0 and stats["bindings"] == 0)
    return observed


async def exercise(binary):
    assert AsyncWatch is rh.AsyncWatch and WatchDelivery is rh.WatchDelivery and WatchControl is rh.WatchControl
    assert Path(rh.__file__).resolve().is_relative_to(Path(sys.prefix).resolve())
    with Fixture(binary) as f:
        parent = rh.AsyncHaystackClient(f.url, "user", "pencil", timeout=3)
        await parent.connect()
        sibling = parent.watch(["parent"], key="sibling")
        retained_sibling = await anext(sibling)
        watch = parent.watch(["a", "b"], key="installed", lease_ms=5000)
        async with watch as same:
            assert same is watch and watch.__aiter__() is watch
            first = await asyncio.ensure_future(watch.next())
            observed = {"initial": snapshot(first)}
            expected_initial = copy.deepcopy(f.command("stats")["deliveries"][-1])
            before = f.command("stats")["log"]
            try:
                watch.next()
            except rh.ClientError:
                pass
            else:
                raise AssertionError("next-before-ACK accepted")
            observed["extra_dispatch_after_rejection"] = f.command("stats")["log"] != before
            for field, value in (("token", "changed"), ("through", 0), ("changed", ())):
                try:
                    setattr(first, field, value)
                except AttributeError:
                    pass
                else:
                    raise AssertionError(f"mutable delivery identity: {field}")
            a = next(row for row in first.changed if row.ref_val == "a")
            a.set("value", 999)
            a["nested"][0].set("value", 999)
            a["nested"].push("caller value")
            assert snapshot(first) == observed["initial"]
            observed["replay"] = snapshot(await watch.resume())
            f.command("set", lose_ack=1)
            uncertain = await watch.ack(first)
            observed["controls"] = {"ack_loss": [uncertain.kind, uncertain.reason]}
            assert watch.pending.token == first.token
            reconciled = await watch.resume()
            assert reconciled.kind == "acknowledged" and reconciled.token == first.token
            assert watch.pending is None
            f.command("update", id="a", value=15)
            f.command("remove_tag", id="a", tag="old")
            f.command("remove", id="b")
            second = await anext(watch)
            observed["update"] = snapshot(second)
            expected_update = copy.deepcopy(f.command("stats")["deliveries"][-1])
            assert (await watch.ack(second)).token == second.token
            assert (await watch.renew(5000)).kind == "renewed"
        report = await watch.aclose()
        assert report.kind == "closed" and report.local_joined and watch.closed
        stats = await f.wait(lambda s: s["active"] == 1 and s["ws_active"] == 1)
        observed.update(child_active=stats["active"] - 1, sibling_active=stats["active"])
        assert (await sibling.ack(retained_sibling)).kind == "acknowledged"
        assert len(await parent.read('id==@parent')) == 1
        calls = [x for x in stats["log"] if x["key"] == "installed"]
        observed["ops"] = [x["op"] for x in calls]
        observed["session_count"] = len({x["session"] for x in calls})
        observed["exact_ack_retry"] = calls[2]["request"] == calls[3]["request"]
        # Compare immutable ACK fields with the actual prepared response receipt.
        ack = rh.decode_typed_value(calls[2]["request"])["body"]
        assert ack["token"] == first.token and ack["through"] == str(first.through)
        assert ack["scopeGeneration"] == str(first.scope_generation)
        await parent.close()
        stats = await f.wait(lambda s: s["active"] == 0 and s["bindings"] == 0 and s["ws_active"] == 0)
        observed.update(parent_active=stats["active"], parent_bindings=stats["bindings"], parent_sockets=stats["ws_active"])
    for reason in ("gap", "overflow", "lease_expired"):
        observed["controls"][reason] = await terminal(binary, reason)
    # Successful HTTP Create followed by rejected WS attach retains exact cleanup.
    with Fixture(binary) as f:
        async with rh.AsyncHaystackClient(f.url, "user", "pencil", timeout=3) as parent:
            f.command("set", reject_ws=True)
            failed = parent.watch(["b", "a", "a"], key="attach-failure")
            assert (await failed.ready()).kind == "unknown"
            assert (await failed.aclose()).kind == "closed"
            stats = f.command("stats")
            calls = [x for x in stats["log"] if x["key"] == "attach-failure"]
            assert [x["op"] for x in calls] == ["watchSub", "watchUnsub"]
            assert len({x["session"] for x in calls}) == 1 and stats["active"] == 0
    # A direct real-router path separately qualifies ordinary WS traffic.
    # Response-loss and request transcript assertions above use the labeled relay.
    with Fixture(binary, direct=True) as f:
        async with rh.AsyncHaystackClient(f.url, "user", "pencil", timeout=3) as parent:
            async with parent.watch(["a"], key="direct") as watch:
                first = await anext(watch)
                assert snapshot(first)["changed"] == [rich_row(1, old=True)]
                await watch.ack(first)
                f.command("update", id="a", value=9)
                second = await anext(watch)
                assert snapshot(second)["changed"] == [rich_row(9, old=True)]
                assert second.from_revision == first.through < second.through
        await f.wait(lambda s: s["active"] == 0 and s["bindings"] == 0)
    observed["key_ownership"] = await ownership_controls(binary)
    observed["cleanup_budget"] = await cleanup_budget_controls(binary)
    oracle(observed, expected_initial, expected_update)
    mutants = rejecting_controls(observed, expected_initial, expected_update)
    import rusty_haystack._watch as helper
    helper_path = Path(helper.__file__).resolve()
    assert helper_path.parent == Path(rh.__file__).resolve().parent
    return {"installed_package": str(Path(rh.__file__).resolve()), "entry_helper": str(helper_path), "entry_helper_sha256": hashlib.sha256(helper_path.read_bytes()).hexdigest(), "fixture_sha256": hashlib.sha256(Path(binary).read_bytes()).hexdigest(), "mutants_rejected": mutants, "direct_real_router_ws": True, "real_server_typed_progression": True, "explicit_ack_resume_renew": True, "single_session_transcript": True, "owned_child_and_parent_cleanup": True, "attach_failure_cleanup": True, "key_ownership_and_cleanup_budget": True, "observation": observed, "scope": "selected-host installed CPython wheel; first-party HTTP and same-authority WS relay preserve real responses; explicitly labeled auxiliary Idle seeding uses authenticated in-process real-router calls"}


async def bounded(binary):
    async with asyncio.timeout(30):
        return await exercise(binary)

parser = argparse.ArgumentParser()
parser.add_argument("--fixture", type=Path, required=True)
args = parser.parse_args()
print(json.dumps(asyncio.run(bounded(args.fixture.resolve())), sort_keys=True))
