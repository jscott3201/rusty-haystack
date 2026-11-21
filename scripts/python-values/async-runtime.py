"""Independent socket consumer, copied beside its fixture outside the checkout."""
import asyncio
import contextvars
import json
from pathlib import Path
import runpy
import ssl
import tempfile

import rusty_haystack as rh
from rusty_haystack.client import AsyncHaystackClient, TlsConfig

# -I excludes script-directory imports; load only the explicitly copied fixture.
fixture = runpy.run_path(str(Path(__file__).with_name("async_peer.py")))
Peer, tls_material = fixture["Peer"], fixture["tls_material"]


async def exercise():
    caller = contextvars.ContextVar("installed-caller")
    async with Peer() as peer:
        client = rh.AsyncHaystackClient(peer.url, "user", "password")
        assert type(client) is AsyncHaystackClient
        async with client as entered:
            assert entered is client
            async def read(value):
                caller.set(value)
                future = client.read("site", limit=1)
                assert isinstance(future, asyncio.Future)
                assert len(await asyncio.ensure_future(future)) == 1
                assert caller.get() == value
            await asyncio.gather(read("one"), read("two"))
        assert client.closed
        await peer.wait_eof()
        await peer.wait_idle()
    async with Peer(mode="stall_read") as peer:
        client = rh.AsyncHaystackClient(peer.url, "user", "password")
        await client.connect()
        pending = client.read("site")
        await peer.wait_requests(4)
        pending.cancel()
        try:
            await pending
        except asyncio.CancelledError:
            pass
        else:
            raise AssertionError("native read did not preserve cancellation")
        await peer.wait_eof()
        await client.close()
        await peer.wait_idle()
    with tempfile.TemporaryDirectory() as temporary:
        directory = tls_material(Path(temporary))
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(directory / "localhost.pem", directory / "localhost-key.pem")
        tls = TlsConfig.with_ca((directory / "ca.pem").read_bytes())
        async with Peer(ssl=context) as peer:
            async with AsyncHaystackClient(peer.url, "user", "password", tls=tls) as client:
                assert len(await client.read("site")) == 1
            await peer.wait_eof()
            await peer.wait_idle()
    return {"async_import": str(Path(rh.__file__).resolve()), "http_scram_read": True, "cancelled_read_peer_eof": True, "custom_ca_tls_read": True, "closed_owners": True}


async def bounded():
    async with asyncio.timeout(15):
        return await exercise()

print(json.dumps(asyncio.run(bounded()), sort_keys=True))
