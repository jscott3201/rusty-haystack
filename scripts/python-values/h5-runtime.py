"""Installed wheel against its first-party application, using explicit H5."""
import asyncio
import json
from pathlib import Path
import runpy

import rusty_haystack as rh

auth_manager = runpy.run_path(str(Path(__file__).with_name("application_fixture.py")))["auth_manager"]


async def exercise():
    graph = rh.SharedGraph()
    graph.add(rh.HDict({"id": rh.Ref("a"), "exact": rh.Int(2**63 - 1), "float": rh.Float(-0.0), "none": rh.NoneValue(), "buf": rh.Buf(b"\x00\xff")}))
    async with rh.ApplicationBuilder(graph, auth=auth_manager()).start() as app:
        host, port = await app.ready()
        async with rh.AsyncHaystackClient(f"http://{host}:{port}/api", "reader", "password", h5=rh.H5Config()) as client:
            row = await client.read_by_id(rh.Ref("a"))
            assert row["exact"] == rh.Int(2**63 - 1)
            assert row["float"] == rh.Float.from_bits(0x8000000000000000)
            assert row["none"] == rh.NoneValue() and row["buf"] == rh.Buf(b"\x00\xff")
            assert await client.read_by_id(checked=False) is None
            rows = await client.read_by_ids([rh.Ref("a"), rh.Ref("missing"), rh.Ref("a")], checked=False)
            assert len(rows) == 3 and len(rows[1]) == 0
            assert rows[0]["id"] == rows[2]["id"] == rh.Ref("a")
            assert len(await client.read_by_ids([])) == 0
            try:
                await client.read_by_id(rh.Ref("missing"))
            except rh.H5Error as error:
                assert error.status == 404 and error.spec == "sys.api::UnknownEntityErr"
                assert error.payload["status"] == rh.Int(404)
            else:
                raise AssertionError("checked missing read succeeded")
        assert client.closed
    assert app.closed
    return {"module": str(Path(rh.__file__).resolve()), "first_party_http": True, "rich_values": True, "ordered_absence": True, "typed_error": True, "closed_owners": True}


async def bounded():
    async with asyncio.timeout(20):
        return await exercise()


print(json.dumps(asyncio.run(bounded()), sort_keys=True))
