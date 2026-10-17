"""Selected H5 requests, exact values, and native ownership over real HTTP."""
import asyncio

import pytest
import rusty_haystack as rh

from .async_peer import Peer
from .h5_peer import H5Peer


def run(coroutine):
    async def bounded():
        async with asyncio.timeout(20):
            return await coroutine
    return asyncio.run(bounded())


def test_explicit_h5_configuration_and_nullable_signature():
    from rusty_haystack.client import H5Config
    config = H5Config()
    assert config.version == "5" and config.format == "jeto" and config.boxing == "all"
    assert rh.H5Config is H5Config
    for kwargs in ({"version": "4"}, {"format": "hayson"}, {"boxing": "none"}):
        with pytest.raises(ValueError):
            H5Config(**kwargs)
    async def case():
        async with Peer() as peer:
            async with rh.AsyncHaystackClient(peer.url, "user", "password") as client:
                with pytest.raises(ValueError, match="H5Config"):
                    client.read_by_id()
                with pytest.raises(ValueError, match="H5Config"):
                    client.read_by_ids([])
                assert len(peer.requests) == 3
    run(case())


def assert_rich(row):
    assert isinstance(row, rh.HDict) and row["id"] == rh.Ref("rich")
    assert row["integer"] == rh.Int(2**63 - 1)
    assert row["float"] == rh.Float.from_bits(0x8000000000000000)
    assert row["number"].unit == "kW"
    assert row["none"] == rh.NoneValue() and row["none"] is not None
    assert row["bytes"] == rh.Buf(b"\x00\xff")
    assert row["list"][0] is None and row["list"][1] == rh.Marker()


def test_independent_peer_exact_values_ordered_absence_and_typed_error():
    async def case():
        async with H5Peer() as peer:
            async with rh.AsyncHaystackClient(peer.url, "user", "password", h5=rh.H5Config()) as client:
                future = client.read_by_id(rh.Ref("rich", "display is not lookup identity"))
                assert isinstance(future, asyncio.Future)
                assert_rich(await asyncio.ensure_future(future))
                assert await client.read_by_id(checked=False) is None
                assert await client.read_by_id(None, checked=False) is None
                assert await client.read_by_id(rh.Ref("missing"), checked=False) is None
                rows = await client.read_by_ids([rh.Ref("rich"), rh.Ref("missing"), rh.Ref("rich")], checked=False)
                assert len(rows) == 3 and len(rows[1]) == 0
                assert_rich(rows[0]); assert_rich(rows[2])
                assert len(await client.read_by_ids([])) == 0
                with pytest.raises(rh.H5Error) as missing_id:
                    await client.read_by_id()
                assert missing_id.value.spec == "sys.api::UnknownEntityErr"
                assert all(len(row) == 0 for row in await client.read_by_ids([rh.Ref("missing")] * 2, checked=False))
                with pytest.raises(rh.H5Error) as error:
                    await client.read_by_ids([rh.Ref("rich"), rh.Ref("missing")])
                assert isinstance(error.value, rh.ClientError)
                assert error.value.status == 404 and error.value.spec == "sys.api::UnknownEntityErr"
                assert error.value.payload["status"] == rh.Int(404)
                assert error.value.payload["dis"] == "private-peer-sentinel"
                assert "private-peer-sentinel" not in str(error.value)
                peer.mode = "permission"
                with pytest.raises(rh.H5Error) as error:
                    await client.read_by_id(rh.Ref("rich"))
                assert error.value.status == 403 and error.value.spec == "sys.api::PermissionErr"
                assert error.value.payload["status"] == rh.Int(418)
            await peer.wait_eof(); await peer.wait_idle()
    run(case())


@pytest.mark.parametrize("mode", ["batch_extra", "batch_swapped", "checked_placeholder"])
def test_batch_cardinality_order_and_checked_placeholders_are_validated(mode):
    async def case():
        async with H5Peer(mode=mode) as peer:
            async with rh.AsyncHaystackClient(peer.url, "user", "password", h5=rh.H5Config()) as client:
                ids = [rh.Ref("rich"), rh.Ref("missing")] if mode == "batch_swapped" else [rh.Ref("rich")]
                with pytest.raises(rh.ClientError, match="identities"):
                    await client.read_by_ids(ids, checked=mode == "checked_placeholder")
            await peer.wait_idle()
    run(case())


@pytest.mark.parametrize("mode", ["unknown_error", "invalid_error", "unknown_value", "wrong_shape", "wrong_identity", "wrong_media", "wrong_version", "duplicate_box", "partial_status", "content_encoding", "invalid_json", "oversize_declared", "oversize_chunked"])
def test_bad_response_is_bounded_and_never_flattens_private_text(mode):
    async def case():
        async with H5Peer(mode=mode) as peer:
            async with rh.AsyncHaystackClient(peer.url, "user", "password", h5=rh.H5Config()) as client:
                with pytest.raises(rh.ClientError) as error:
                    await client.read_by_id(rh.Ref("rich"))
                assert not isinstance(error.value, rh.H5Error)
                assert "private-peer-sentinel" not in str(error.value)
            await peer.wait_eof(); await peer.wait_idle()
    run(case())


@pytest.mark.parametrize("verify", [True, False])
def test_h5_reuses_original_session_tls_configuration(verify, tmp_path):
    import ssl
    from .async_peer import tls_material
    from rusty_haystack.client import TlsConfig
    directory = tls_material(tmp_path)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(directory / "localhost.pem", directory / "localhost-key.pem")
    tls = TlsConfig.with_ca((directory / "ca.pem").read_bytes()) if verify else None
    async def case():
        async with H5Peer(ssl=context) as peer:
            async with rh.AsyncHaystackClient(peer.url, "user", "password", tls=tls, tls_verify=verify, h5=rh.H5Config()) as client:
                assert_rich(await client.read_by_id(rh.Ref("rich")))
            await peer.wait_eof(); await peer.wait_idle()
    run(case())


def test_invalid_arguments_do_not_dispatch_and_conversion_failure_releases_admission():
    async def case():
        async with H5Peer(mode="submicrosecond") as peer:
            async with rh.AsyncHaystackClient(peer.url, "user", "password", h5=rh.H5Config()) as client:
                for bad in ("rich", 1, rh.Int(1)):
                    with pytest.raises(TypeError):
                        client.read_by_id(bad)
                    with pytest.raises(TypeError):
                        client.read_by_ids([bad])
                for ids in ([rh.Ref("x")] * 1001, [rh.Ref("é" * 4097)]):
                    with pytest.raises(ValueError):
                        client.read_by_ids(ids)
                for deadline in (float("inf"), float("nan")):
                    with pytest.raises(ValueError):
                        client.read_by_id(deadline=deadline)
                with pytest.raises(TypeError):
                    client.read_by_id(checked="false")
                with pytest.raises(rh.ClientError, match="bounds"):
                    await client.read_by_ids([rh.Ref("x" * 8192)] * 130)
                assert len(peer.requests) == 3
                for _ in range(20):
                    with pytest.raises(ValueError, match="microsecond"):
                        await client.read_by_id(rh.Ref("rich"))
                peer.mode = "normal"
                assert_rich(await client.read_by_id(rh.Ref("rich")))
            await peer.wait_idle()
    run(case())


def test_id_bounds_precede_sequence_extraction_and_owned_copies():
    class UnvisitedList(list):
        def __iter__(self):
            raise AssertionError("unbounded sequence extraction")

    async def case():
        async with H5Peer() as peer:
            async with rh.AsyncHaystackClient(peer.url, "user", "password", h5=rh.H5Config()) as client:
                # Small fixture; a million aliases is unnecessary and unsafe.
                oversized = UnvisitedList([rh.Ref("x")] * 1001)
                with pytest.raises(ValueError, match="1000"):
                    client.read_by_ids(oversized)
                with pytest.raises(ValueError, match="8192"):
                    client.read_by_ids([rh.Ref("x" * 8193), object()])
                with pytest.raises(ValueError, match="8192"):
                    client.read_by_id(rh.Ref("é" * 4097))
                with pytest.raises(TimeoutError):
                    await client.read_by_id(rh.Ref("rich"), deadline=asyncio.get_running_loop().time() - 1)
                assert len(peer.requests) == 3
                assert_rich(await client.read_by_id(rh.Ref("rich")))
            await peer.wait_idle()
    run(case())


@pytest.mark.parametrize("ending", ["cancel", "deadline", "close"])
@pytest.mark.parametrize("many", [False, True])
def test_native_h5_lifecycle_ends_stalled_socket_and_allows_reuse(ending, many):
    async def case():
        async with H5Peer(mode="stall_h5") as peer:
            client = rh.AsyncHaystackClient(peer.url, "user", "password", h5=rh.H5Config())
            await client.connect()
            method, argument = (client.read_by_ids, [rh.Ref("rich")]) if many else (client.read_by_id, rh.Ref("rich"))
            kwargs = {"deadline": asyncio.get_running_loop().time() + .1} if ending == "deadline" else {}
            pending = method(argument, **kwargs)
            await peer.wait_requests(4)
            if ending == "cancel":
                pending.cancel()
                with pytest.raises(asyncio.CancelledError):
                    await pending
            elif ending == "deadline":
                with pytest.raises(TimeoutError):
                    await pending
            else:
                await client.close()
                with pytest.raises(rh.ClientError):
                    await pending
            await peer.wait_eof()
            if ending != "close":
                peer.mode = "normal"
                assert_rich(await client.read_by_id(rh.Ref("rich")))
            await client.close(); await client.close()
            assert client.closed
            with pytest.raises(rh.ClientError):
                client.read_by_id()
            await peer.wait_idle()
    run(case())


def test_first_party_application_rich_read_and_explicit_unrepresentable_error():
    from .application_fixture import auth_manager
    graph = rh.SharedGraph()
    graph.add(rh.HDict({"id": rh.Ref("rich"), "integer": rh.Int(2**63 - 1), "float": rh.Float(-0.0), "number": rh.Number(-0.0, "kW"), "none": rh.NoneValue(), "bytes": rh.Buf(b"\x00\xff"), "list": rh.HList([None, rh.Marker()])}))
    graph.add(rh.HDict({"id": rh.Ref("unsupported"), "value": rh.Float.from_bits(0x7ff8000000000001)}))
    async def case():
        async with rh.ApplicationBuilder(graph, auth=auth_manager()).start() as app:
            host, port = await app.ready()
            async with rh.AsyncHaystackClient(f"http://{host}:{port}/api", "reader", "password", h5=rh.H5Config()) as client:
                assert_rich(await client.read_by_id(rh.Ref("rich")))
                assert await client.read_by_id(checked=False) is None
                rows = await client.read_by_ids([rh.Ref("rich"), rh.Ref("missing"), rh.Ref("rich")], checked=False)
                assert len(rows) == 3 and len(rows[1]) == 0
                assert_rich(rows[0]); assert_rich(rows[2])
                with pytest.raises(rh.H5Error) as error:
                    await client.read_by_id(rh.Ref("unsupported"))
                assert error.value.status == 406 and error.value.spec == "sys.api::NotAcceptableErr"
    run(case())
