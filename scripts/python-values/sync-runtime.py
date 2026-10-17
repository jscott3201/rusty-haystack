"""Exact installed-wheel blocking consumer with an independent Python peer."""
import asyncio
import inspect
import json
from pathlib import Path
import runpy
import ssl
import tempfile
import threading

import rusty_haystack as rh
from rusty_haystack.client import SyncHaystackClient, HaystackClient, WsClient, TlsConfig

fixture = runpy.run_path(str(Path(__file__).with_name("sync_peer.py")))
ThreadedPeer = fixture["ThreadedPeer"]
tls_material = runpy.run_path(str(Path(__file__).with_name("async_peer.py")))["tls_material"]

assert rh.SyncHaystackClient is SyncHaystackClient
exit_signature = inspect.signature(SyncHaystackClient.__exit__)
assert list(exit_signature.parameters) == ["self", "exc_type", "exc_value", "traceback"]
assert all(hasattr(HaystackClient, name) for name in ("about", "ops", "formats", "libs", "read_by_ids", "nav", "defs", "watch_sub", "point_write", "his_read", "his_write", "invoke_action", "call"))
assert hasattr(WsClient, "watch_sub")

with ThreadedPeer() as peer:
    client = SyncHaystackClient(peer.url, "user", "password")
    async def reject_loop():
        try: client.connect()
        except RuntimeError: pass
        else: raise AssertionError("running loop accepted")
    asyncio.run(reject_loop())
    assert client.state == "new" and not peer.requests
    with client as same:
        assert same is client
        result = client.read("site", limit=1, timeout=2.0)
        assert isinstance(result, rh.HGrid) and len(result) == 1
        assert result[0]["id"].val == "r0"
    assert client.closed and client.close() is None
    peer.wait_eof(); peer.wait_idle()

with ThreadedPeer() as peer:
    client = SyncHaystackClient(peer.url, "user", "password")
    client.connect()
    try:
        assert client.__exit__(exc_type=None, exc_value=None, traceback=None) is None
        assert client.closed
        peer.wait_eof(); peer.wait_idle()
    finally:
        client.close()

with ThreadedPeer(mode="stall_read") as peer:
    client = SyncHaystackClient(peer.url, "user", "password")
    assert client.connect() is None
    try: client.read("site", timeout=.05)
    except TimeoutError: pass
    else: raise AssertionError("stalled read accepted")
    peer.wait_eof()
    assert client.state == "open"
    peer.set_mode("normal")
    assert len(client.read("site")) == 1
    client.close(); peer.wait_idle()

with tempfile.TemporaryDirectory() as temporary:
    directory = tls_material(Path(temporary))
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(directory / "localhost.pem", directory / "localhost-key.pem")
    tls = TlsConfig.with_ca((directory / "ca.pem").read_bytes())
    with ThreadedPeer(ssl=context) as peer:
        client = SyncHaystackClient(peer.url, "user", "password", tls=tls)
        errors = []
        def whole_flow():
            try:
                with client: assert len(client.read("site")) == 1
            except BaseException as error: errors.append(error)
        thread = threading.Thread(target=whole_flow); thread.start(); thread.join(8)
        assert not thread.is_alive() and not errors, errors
        assert client.closed
        try: client.close()
        except RuntimeError: pass
        else: raise AssertionError("foreign thread accepted")
        peer.wait_eof(); peer.wait_idle()

print(json.dumps({"sync_import": str(Path(rh.__file__).resolve()), "direct_values": True, "exit_keywords": True, "exit_signature": str(exit_signature), "python_peer_progress": True, "loop_thread_rejection": True, "read_timeout_recovery": True, "custom_ca_tls": True, "closed_owners": True, "legacy_capabilities_preserved": True}, sort_keys=True))
