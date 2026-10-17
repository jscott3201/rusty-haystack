"""Run and type-check this file only against an installed wheel outside the repo."""
import datetime as dt
import json
from pathlib import Path
from typing import Any, AsyncIterator, Coroutine, Literal, assert_type

import rusty_haystack as rh
from rusty_haystack import Buf, CodecError, Coord, Float, HDateTime, HDict, HGrid, HList, Int, Nominal, NoneValue, Number
from rusty_haystack.auth import generate_nonce
from rusty_haystack.client import AsyncHaystackClient, SyncHaystackClient, AsyncWatch, WatchControl, WatchDelivery, HaystackClient, TlsConfig, H5Config, H5Error
from rusty_haystack.codecs import decode_typed_value, encode_typed_value
from rusty_haystack.data import HCol
from rusty_haystack.filter import CmpOp, Filter, Path as FilterPath
from rusty_haystack.graph import DiffOp, EntityGraph, GraphDiff, SharedGraph
from rusty_haystack.kinds import Int as KindInt
from rusty_haystack.ontology import DefNamespace, Slot, Spec
from rusty_haystack.server import ApplicationBuilder, ApplicationHandle, ApplicationOwner, ApplicationTermination, AuthManager, HaystackServer, HisStore
from rusty_haystack.units import compatible

assert KindInt is Int and rh.kinds.Int is Int
assert H5Config is rh.H5Config and H5Error is rh.H5Error
assert issubclass(H5Error, rh.ClientError)
assert_type(H5Config().version, Literal["5"])
assert issubclass(CodecError, rh.HaystackError)
assert all(value is not None for value in (generate_nonce, HaystackClient, TlsConfig, HCol, CmpOp, Filter, FilterPath, DiffOp, GraphDiff, DefNamespace, Slot, Spec, AuthManager, HaystackServer, HisStore, compatible))
number = Number(val=42, unit="kW")
assert_type(number.val, float)
assert_type(number.unit, str | None)
assert_type(Int(2**63 - 1).value, int)
assert_type(Float.from_bits(0x7FF8000000000001).bits, int)
assert_type(Buf(b"\x00\xff").data, bytes)
assert_type(Coord(1, 2).lat, float)
assert_type(Nominal("example::Serial", "A", "r1", "00042").schema_id, str)
assert_type(HDateTime(2024, 1, 1, 0, 0, 0, 3600, "opaque").dt(), dt.datetime)
maybe_number = HDict({"v": number}).get("v")
if isinstance(maybe_number, Number):
    assert_type(maybe_number.val, float)
values = HList([True, 42, 1.5, number, Int(2**63 - 1), Float.from_bits(1), NoneValue(), None, Buf(b"x"), dt.date(2024, 1, 1), dt.time(1, 2)])
entity = HDict({"id": rh.Ref("fixture"), "values": values})
native_dict: dict[str, int] = {"v": 42}
native_list: list[int] = [42]
rich_list: list[Int] = [Int(42)]
assert HDict(native_dict)["v"] == Number(42)
assert HDict.from_dict(native_dict)["v"] == Number(42)
assert HList(native_list)[0] == Number(42)
assert HList(rich_list)[0] == Int(42)
assert HDict(entity.to_dict()) == entity
assert HDict.from_dict(entity.to_dict()) == entity
assert list(HList(entity.values())) == entity.values()
wire = encode_typed_value(entity)
decoded = decode_typed_value(wire)
assert isinstance(decoded, HDict) and decoded == entity
for graph in (EntityGraph(), SharedGraph()):
    graph.add(entity)
    assert graph.get("fixture") == entity
    assert graph.remove("fixture") == entity
fixture = json.loads(Path(__file__).with_name("typed-v1-entity.json").read_text())
assert json.loads(encode_typed_value(decode_typed_value(json.dumps(fixture)))) == fixture
try:
    rh.encode_grid("text/zinc", HGrid.from_parts(HDict(), [], [entity]))
except CodecError:
    pass
else:
    raise AssertionError("rich values were silently projected by an H4 codec")

# The installed client surface is Future-valued, including context management.
# The network consumer below is a separate executable fixture in this gate.
import asyncio

assert AsyncHaystackClient is rh.AsyncHaystackClient
async def async_surface(client: AsyncHaystackClient) -> None:
    connected = client.connect(deadline=asyncio.get_running_loop().time() + 2)
    assert_type(connected, asyncio.Future[None])
    await connected
    read = client.read("site", limit=1)
    assert_type(read, asyncio.Future[HGrid])
    assert_type(await asyncio.ensure_future(read), HGrid)
    assert_type(client.close(), asyncio.Future[None])
    async with rh.AsyncHaystackClient("http://localhost:1", "user", "password") as owned:
        assert_type(owned, AsyncHaystackClient)

async def close_unconnected_owner() -> None:
    client = rh.AsyncHaystackClient("http://localhost:1", "user", "password")
    await client.close()
    assert client.closed

asyncio.run(close_unconnected_owner())

assert SyncHaystackClient is rh.SyncHaystackClient
def sync_surface(client: SyncHaystackClient) -> None:
    assert_type(client.connect(timeout=2.0), None)
    assert_type(client.read("site", limit=100, timeout=1.0), HGrid)
    assert_type(client.close(), None)
    with rh.SyncHaystackClient("http://localhost:1", "user", "password") as owned:
        assert_type(owned, SyncHaystackClient)

sync_client = rh.SyncHaystackClient("http://localhost:1", "user", "password")
assert_type(sync_client.__exit__(exc_type=None, exc_value=None, traceback=None), None)
assert sync_client.closed

assert ApplicationBuilder is rh.ApplicationBuilder
assert ApplicationOwner is rh.ApplicationOwner
assert ApplicationHandle is rh.ApplicationHandle
assert ApplicationTermination is rh.ApplicationTermination
assert issubclass(rh.ApplicationError, rh.HaystackError)

async def application_surface() -> None:
    async with rh.ApplicationBuilder(SharedGraph()).start() as application:
        assert_type(application, ApplicationOwner)
        assert_type(application.ready(), asyncio.Future[tuple[str, int]])
        assert_type(application.handle(), ApplicationHandle)
        assert_type(application.read("site", limit=1), asyncio.Future[HGrid])
        assert_type(await application.read("site"), HGrid)
    report = await application.terminated()
    assert_type(report, ApplicationTermination)
    assert_type(report.close_error, str | None)
    assert_type(report.drain_expired, bool | None)
    assert_type(report.cleanup_errors, tuple[str, ...])
    assert_type(application.close(), asyncio.Future[None])
    await application.close()

async def application_entry_types(application: ApplicationOwner) -> None:
    entering = assert_type(application.__aenter__(), Coroutine[Any, Any, ApplicationOwner])
    assert_type(await entering, ApplicationOwner)
    task = asyncio.create_task(application.__aenter__())
    assert_type(task, asyncio.Task[ApplicationOwner])
    await task

asyncio.run(application_surface())

assert AsyncWatch is rh.AsyncWatch and WatchDelivery is rh.WatchDelivery and WatchControl is rh.WatchControl
async def watch_surface(client: AsyncHaystackClient, ids: list[str]) -> None:
    watch = client.watch(ids, key="consumer", lease_ms=5000, cleanup_timeout=2.0)
    assert_type(watch, AsyncWatch)
    assert_type(watch.__aiter__(), AsyncWatch)
    iterator: AsyncIterator[WatchDelivery | WatchControl] = watch
    assert_type(iterator, AsyncWatch)
    assert_type(watch.ready(), asyncio.Future[WatchControl])
    assert_type(watch.__anext__(), asyncio.Future[WatchDelivery | WatchControl])
    assert_type(watch.resume(), asyncio.Future[WatchDelivery | WatchControl])
    assert_type(watch.renew(5000), asyncio.Future[WatchControl])
    assert_type(watch.aclose(), asyncio.Future[WatchControl])
    assert_type(watch.pending, WatchDelivery | None)
    entry = assert_type(watch.__aenter__(), Coroutine[Any, Any, AsyncWatch])
    assert_type(await entry, AsyncWatch)
    async with watch as entered:
        assert_type(entered, AsyncWatch)
        event = await anext(watch)
        assert_type(event, WatchDelivery | WatchControl)
        if isinstance(event, WatchDelivery):
            assert_type(event.authority, str)
            assert_type(event.token, str)
            assert_type(event.scope_generation, int)
            assert_type(event.from_revision, int)
            assert_type(event.through, int)
            assert_type(event.initial, bool)
            assert_type(event.changed, tuple[HDict, ...])
            assert_type(event.removed, tuple[str, ...])
            for row in event.changed:
                row.set("local", 1)
            assert_type(await asyncio.ensure_future(watch.ack(event)), WatchControl)
        else:
            assert_type(event.reason, str | None)
            assert_type(event.local_joined, bool)
    async def one() -> WatchDelivery | WatchControl:
        return await watch.next()
    async with asyncio.TaskGroup() as group:
        assert_type(group.create_task(one()), asyncio.Task[WatchDelivery | WatchControl])

async def h5_surface(client: AsyncHaystackClient) -> None:
    assert_type(client.read_by_id(), asyncio.Future[HDict | None])
    assert_type(client.read_by_id(None, checked=False), asyncio.Future[HDict | None])
    assert_type(client.read_by_id(rh.Ref("a")), asyncio.Future[HDict | None])
    assert_type(client.read_by_ids([rh.Ref("a")]), asyncio.Future[HGrid])
    assert_type(await client.read_by_ids([], checked=False), HGrid)
    try:
        await client.read_by_id()
    except H5Error as error:
        assert_type(error.spec, str)
        assert_type(error.status, int)
        assert_type(error.payload, HDict)
