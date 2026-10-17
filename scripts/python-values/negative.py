"""Each marked line must produce a real static error in the installed consumer."""
import rusty_haystack as rh

number = rh.Number(1)
coord = rh.Coord(0, 0)
number.val = 2.0  # reject
number.unit = "kW"  # reject
coord.lat = 1.0  # reject
rh.Int(1).value = 2  # reject
rh.Float(1.0).bits = 0  # reject
rh.Buf(b"x").data = b"y"  # reject
rh.Nominal("a::B", "a", "r", "x").value = "y"  # reject
rh.Number("bad")  # reject
rh.Coord("bad", 0)  # reject
rh.Int("bad")  # reject
rh.Float.from_bits("bad")  # reject
rh.Buf([1, 2])  # reject
rh.HDict({"bad": object()})  # reject
rh.HList([object()])  # reject
rh.encode_typed_value(object())  # reject
rh.HaystackClient  # reject: lives only in the client submodule

import asyncio
async_client = rh.AsyncHaystackClient("http://localhost:1", "user", "password")
rh.H5Config(version="4")  # reject
rh.H5Config(format="hayson")  # reject
rh.H5Config(boxing="none")  # reject
rh.H5Config().version = "5"  # reject
rh.AsyncHaystackClient("http://localhost:1", "user", "password", h5=True)  # reject
async_client.read_by_id("a")  # reject
async_client.read_by_ids(["a"])  # reject
async_client.read_by_ids([None])  # reject
async_client.read_by_id(None, False)  # reject
async_client.read_by_id(checked="false")  # reject
asyncio.create_task(async_client.read_by_id())  # reject
async_client.read("site", limit=None)  # reject
async_client.read("site", deadline="later")  # reject
async_client.close(1.0)  # reject
async_client.closed = True  # reject
async_client.state = "open"  # reject
asyncio.create_task(async_client.read("site"))  # reject: Future is not a coroutine
rh.AsyncHaystackClient("http://localhost:1", "user", "password", timeout="slow")  # reject

builder = rh.ApplicationBuilder()
builder.start(1)  # reject
rh.ApplicationBuilder(host=123)  # reject
rh.ApplicationBuilder(auth=object())  # reject
application = builder.start()
application.read("site", cursor=123)  # reject
application.read("site", limit=None)  # reject
application.close(1.0)  # reject
application.closed = True  # reject
application.handle().close()  # reject: borrowed view has no shutdown authority
asyncio.create_task(application.ready())  # reject: Future needs an async wrapper
application.__aenter__().cancel()  # reject: entry is a coroutine, not a Future

async def bad_report() -> None:
    report = await application.terminated()
    report.close_error = None  # reject
    report.cleanup_errors = ()  # reject

watch = async_client.watch(["a"], key="negative")
async_client.watch("a", key="bad")  # reject
async_client.watch([1], key="bad")  # reject
async_client.watch(["a"], key=1)  # reject
async_client.watch(["a"], key="bad", lease_ms="long")  # reject
async_client.watch(["a"], key="bad", cleanup_timeout="long")  # reject
watch.next(deadline="later")  # reject
watch.ack(object())  # reject
watch.renew("long")  # reject
watch.key = "changed"  # reject
watch.pending = None  # reject
watch.closed = True  # reject
asyncio.create_task(watch.next())  # reject
watch.__aenter__().cancel()  # reject
async def bad_watch(delivery: rh.WatchDelivery, control: rh.WatchControl) -> None:
    await watch  # reject
    delivery.token = "changed"  # reject
    delivery.scope_generation = 0  # reject
    delivery.through = 0  # reject
    delivery.changed = ()  # reject
    delivery.removed = ()  # reject
    control.kind = "closed"  # reject
    control.local_joined = True  # reject
