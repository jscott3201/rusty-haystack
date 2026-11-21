"""Installed-only embedded/HTTP parity and application cleanup consumer."""
import asyncio
import hashlib
import json
from pathlib import Path
import runpy
import socket

import rusty_haystack as rh

fixture = runpy.run_path(str(Path(__file__).with_name("application_fixture.py")))
auth_manager, ids, request = fixture["auth_manager"], fixture["ids"], fixture["request"]
cancel_entry_after_completion = fixture["cancel_entry_after_completion"]


async def exercise():
    graph = rh.SharedGraph.from_grid(rh.decode_grid("text/zinc", 'ver:"3.0"\nid,site,dis\n@a,M,"Alpha"\n@b,M,"Beta"\n'))
    async with rh.ApplicationBuilder(graph, auth=auth_manager()).start() as app:
        handle = app.handle()
        address = await app.ready()
        assert address and address[0] == "127.0.0.1" and address[1] > 0
        url = f"http://{address[0]}:{address[1]}/api"
        async with rh.AsyncHaystackClient(url, "reader", "password") as client:
            assert ids(await client.read("site")) == ids(await handle.read("site")) == ["a", "b"]
        async with rh.AsyncHaystackClient(url, "denied", "password") as client:
            try:
                await client.read("site")
            except rh.ClientError:
                pass
            else:
                raise AssertionError("HTTP identity without read authority disclosed entities")
        first = await app.read("site", limit=1)
        assert not first.meta()["complete"]
        second = await app.read("site", limit=1, cursor=first.meta()["cursor"])
        assert second.meta()["complete"] and ids(first) + ids(second) == ["a", "b"]
        reader, writer = await asyncio.open_connection(*address)
        writer.write(b"GET /api/ops HTTP/1.1\r\nHost: localhost\r\n\r\n")
        await writer.drain()
        # This response proves native accept happened while the peer stays open.
        assert b"200" in await reader.readuntil(b"\r\n\r\n")
    assert app.closed and handle.closed
    assert (await handle.terminated()).close_error is None
    while await reader.read(4096):
        pass
    with socket.socket() as rebound:
        rebound.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        rebound.bind(address)
        rebound.listen()
    writer.close()
    await writer.wait_closed()
    assert ids(graph.read("site")) == ["a", "b"]
    async with rh.ApplicationBuilder(graph).start() as next_app:
        assert ids(await next_app.read("site")) == ["a", "b"]
        address = await next_app.ready()
        status, body = await asyncio.to_thread(request, address, "/api/read", 'ver:"3.0"\nfilter\n"site"\n')
        assert status == 403 and b"Alpha" not in body
    cancelled_app = rh.ApplicationBuilder(graph).start()
    cancelled_address = await cancelled_app.ready()
    try:
        await cancel_entry_after_completion(cancelled_app, catch_cancellation=True)
        with socket.socket() as rebound:
            rebound.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            rebound.bind(cancelled_address)
            rebound.listen()
    finally:
        await cancelled_app.close()
    # Context entry above must load the private helper from this installed wheel.
    import rusty_haystack._application as application_helper
    helper = Path(application_helper.__file__).resolve()
    assert helper.parent == Path(rh.__file__).resolve().parent
    return {
        "installed_package": str(Path(rh.__file__).resolve()),
        "embedded_authenticated_http_parity": True,
        "anonymous_and_no_read_denied": True,
        "native_pagination": True,
        "accepted_socket_eof": True,
        "exact_port_reused": True,
        "borrowed_graph_and_bridge_usable": True,
        "cancelled_entry_after_completed_future": True,
        "entry_helper": str(helper),
        "entry_helper_sha256": hashlib.sha256(helper.read_bytes()).hexdigest(),
        "scope": "in-memory native scoped read service and lifecycle; not legacy CLI capability parity or PostgreSQL qualification",
    }


async def bounded():
    async with asyncio.timeout(20):
        return await exercise()

print(json.dumps(asyncio.run(bounded()), sort_keys=True))
