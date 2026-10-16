"""Real asyncio/native ownership regressions; no pytest-asyncio dependency."""
import asyncio
import contextvars

import pytest
import rusty_haystack as rh

from .async_peer import Peer, tls_material


def run(coroutine):
    async def bounded():
        async with asyncio.timeout(15):
            await coroutine
    asyncio.run(bounded())


def test_async_owner_exists_without_a_loop_or_io():
    client = rh.AsyncHaystackClient("http://localhost:1/api", "user", "password")
    assert client.state == "new" and not client.closed
    with pytest.raises(RuntimeError):
        client.connect()
    assert client.state == "new"


def test_real_scram_read_and_context_close():
    async def case():
        async with Peer() as peer:
            client = rh.AsyncHaystackClient(peer.url, "user", "password")
            async with client as same:
                assert same is client and client.state == "open"
                future = client.read("site")
                assert isinstance(future, asyncio.Future)
                assert len(await asyncio.ensure_future(future)) == 1
            assert client.state == "closed" and client.closed
            await peer.wait_eof()
            await peer.wait_idle()
            await client.close()
            with pytest.raises(rh.ClientError):
                client.read("site")
            with pytest.raises(rh.ClientError):
                client.connect()
    run(case())


@pytest.mark.parametrize("operation", ["connect", "read"])
@pytest.mark.parametrize("use_tls", [False, True])
def test_cancelled_native_operation_releases_peer(operation, use_tls, tmp_path):
    import ssl
    from rusty_haystack.client import TlsConfig
    context, tls = None, None
    if use_tls:
        directory = tls_material(tmp_path)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(directory / "localhost.pem", directory / "localhost-key.pem")
        tls = TlsConfig.with_ca((directory / "ca.pem").read_bytes())
    async def case():
        async with Peer(mode="stall_" + operation, ssl=context) as peer:
            client = rh.AsyncHaystackClient(peer.url, "user", "password", tls=tls)
            if operation == "read":
                await client.connect()
                pending = client.read("site")
                await peer.wait_requests(4)
            else:
                pending = client.connect()
                await peer.wait_requests(1)
            pending.cancel()
            with pytest.raises(asyncio.CancelledError):
                await pending
            await peer.wait_eof()
            if operation == "read":
                peer.mode = "normal"
                assert len(await client.read("site")) == 1
            await client.close()
            assert client.closed
            await peer.wait_idle()
    run(case())


def test_close_races_with_read_and_cancelled_close_waiter():
    async def case():
        async with Peer(mode="stall_read") as peer:
            client = rh.AsyncHaystackClient(peer.url, "user", "password")
            await client.connect()
            pending = client.read("site")
            await peer.wait_requests(4)
            close = client.close()
            assert client.state in ("closing", "closed")
            close.cancel()
            with pytest.raises(asyncio.CancelledError):
                await close
            with pytest.raises(rh.ClientError):
                client.read("site")
            await asyncio.gather(client.close(), client.close())
            with pytest.raises(rh.ClientError):
                await pending
            assert client.closed
            await peer.wait_eof()
            await peer.wait_idle()
    run(case())


def test_absolute_read_deadline_cancels_native_io():
    async def case():
        async with Peer(mode="stall_read") as peer:
            client = rh.AsyncHaystackClient(peer.url, "user", "password")
            await client.connect()
            with pytest.raises(TimeoutError):
                await client.read("site", deadline=asyncio.get_running_loop().time() + .05)
            await peer.wait_eof()
            await client.close()
    run(case())


@pytest.mark.parametrize("mode", ["oversize_declared", "oversize_chunked", "oversize_unknown", "excess_rows", "error_status", "error_grid", "invalid_grid"])
def test_bounded_or_invalid_response_is_rejected(mode):
    async def case():
        async with Peer(mode=mode) as peer:
            async with rh.AsyncHaystackClient(peer.url, "user", "password") as client:
                with pytest.raises(rh.ClientError) as error:
                    await client.read("site", limit=1)
                assert "private-peer-sentinel" not in str(error.value)
            await peer.wait_idle()
    run(case())


def test_bounds_reject_before_dispatch_and_context_is_preserved():
    var = contextvars.ContextVar("caller")
    async def case():
        async with Peer() as peer:
            async with rh.AsyncHaystackClient(peer.url, "user", "password") as client:
                for limit in [0, -1, 1001, 2**100]:
                    with pytest.raises((ValueError, OverflowError)):
                        client.read("site", limit=limit)
                for oversized in ["x" * 8193, "é" * 4097]:
                    with pytest.raises(ValueError):
                        client.read(oversized)
                for deadline in [float("nan"), float("inf")]:
                    with pytest.raises(ValueError):
                        client.read("site", deadline=deadline)
                assert len(peer.requests) == 3
                async def caller(value):
                    var.set(value)
                    await client.read("site")
                    assert var.get() == value
                await asyncio.gather(caller("one"), caller("two"))
            await peer.wait_idle()
    run(case())


def test_expired_close_seals_admission_and_repeated_close_joins():
    async def case():
        async with Peer(mode="stall_read") as peer:
            client = rh.AsyncHaystackClient(peer.url, "user", "password")
            await client.connect()
            pending = client.read("site")
            await peer.wait_requests(4)
            close = client.close(deadline=asyncio.get_running_loop().time() - 1)
            assert client.state in ("closing", "closed")
            with pytest.raises(rh.ClientError):
                client.read("site")
            with pytest.raises(TimeoutError):
                await close
            await client.close()
            with pytest.raises(rh.ClientError):
                await pending
            assert client.closed
            await peer.wait_eof()
            await peer.wait_idle()
    run(case())


def test_invalid_close_deadline_preserves_open_owner():
    async def case():
        async with Peer() as peer:
            async with rh.AsyncHaystackClient(peer.url, "user", "password") as client:
                with pytest.raises(ValueError):
                    client.close(deadline=float("nan"))
                assert client.state == "open"
                assert len(await client.read("site")) == 1
    run(case())


def test_admission_is_finite_and_cancelled_slots_are_reusable():
    async def case():
        async with Peer(mode="stall_read") as peer:
            async with rh.AsyncHaystackClient(peer.url, "user", "password") as client:
                reads = [client.read("site") for _ in range(16)]
                with pytest.raises(rh.ClientError, match="too many in-flight"):
                    client.read("site")
                await peer.wait_requests(19)
                for pending in reads:
                    pending.cancel()
                await asyncio.gather(*reads, return_exceptions=True)
                await peer.wait_eof(16)
                peer.mode = "normal"
                # Cancellation notification and bridge-guard destruction are
                # asynchronous. Reuse is admitted once their real guards drain.
                for _ in range(100):
                    try:
                        resumed = client.read("site")
                        break
                    except rh.ClientError:
                        await asyncio.sleep(.01)
                else:
                    raise AssertionError("cancelled admission never drained")
                assert len(await resumed) == 1
            await peer.wait_idle()
    run(case())


def test_affinity_rejected_on_foreign_thread_and_later_loop_including_closed():
    import threading
    client = rh.AsyncHaystackClient("http://localhost:1/api", "user", "password")
    async def bind_and_close():
        await client.close()
        errors = []
        def foreign_thread():
            async def case():
                for method in (client.close, client.connect, lambda: client.read("site")):
                    try:
                        method()
                    except RuntimeError as error:
                        errors.append(str(error))
            asyncio.run(case())
        thread = threading.Thread(target=foreign_thread)
        thread.start()
        await asyncio.to_thread(thread.join, 3)
        assert not thread.is_alive() and len(errors) == 3
    run(bind_and_close())
    async def another_loop():
        with pytest.raises(RuntimeError, match="different event loop"):
            client.close()
        assert client.closed
    run(another_loop())


def test_supported_future_patterns_and_responsive_event_loop():
    async def case():
        async with Peer(mode="stall_read") as peer:
            client = rh.AsyncHaystackClient(peer.url, "user", "password")
            await asyncio.wait_for(client.connect(), 2)
            ticks = 0
            async def ticker():
                nonlocal ticks
                for _ in range(10):
                    ticks += 1
                    await asyncio.sleep(.005)
            tick = asyncio.create_task(ticker())
            with pytest.raises(TimeoutError):
                await asyncio.wait_for(client.read("site"), .1)
            await tick
            assert ticks == 10
            await peer.wait_eof()
            peer.mode = "normal"
            async def wrapped():
                return await client.read("site")
            assert len(await asyncio.create_task(wrapped())) == 1
            async with asyncio.TaskGroup() as tasks:
                task = tasks.create_task(wrapped())
            assert len(task.result()) == 1
            await client.close()
    run(case())


def test_original_context_body_error_survives_cleanup():
    class BodyError(Exception):
        pass
    async def case():
        async with Peer() as peer:
            client = rh.AsyncHaystackClient(peer.url, "user", "password")
            original = BodyError("body-failed")
            with pytest.raises(BodyError) as caught:
                async with client:
                    raise original
            assert caught.value is original and client.closed
            await peer.wait_eof()
    run(case())


def test_repeated_contexts_and_drop_of_open_owner_release_peers():
    import gc
    async def case():
        async with Peer() as peer:
            for _ in range(5):
                async with rh.AsyncHaystackClient(peer.url, "user", "password") as client:
                    assert len(await client.read("site")) == 1
                await peer.wait_idle()
            client = rh.AsyncHaystackClient(peer.url, "user", "password")
            await client.connect()
            del client
            gc.collect()
            await peer.wait_eof(6)
            await peer.wait_idle()
    run(case())


@pytest.mark.parametrize("mutual", [False, True])
def test_real_tls_and_optional_mutual_identity(mutual, tmp_path):
    import ssl
    from rusty_haystack.client import TlsConfig
    directory = tls_material(tmp_path)
    cert, key = directory / "localhost.pem", directory / "localhost-key.pem"
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(cert, key)
    if mutual:
        context.verify_mode = ssl.CERT_REQUIRED
        context.load_verify_locations(cafile=directory / "ca.pem")
    tls = TlsConfig.from_files(str(cert), str(key), str(directory / "ca.pem")) if mutual else TlsConfig.with_ca((directory / "ca.pem").read_bytes())
    async def case():
        async with Peer(ssl=context) as peer:
            async with rh.AsyncHaystackClient(peer.url, "user", "password", tls=tls) as client:
                assert len(await client.read("site")) == 1
            await peer.wait_eof()
            await peer.wait_idle()
    run(case())


def test_tls_untrusted_and_wrong_hostname_are_rejected(tmp_path):
    import ssl
    from rusty_haystack.client import TlsConfig
    directory = tls_material(tmp_path)
    cert, key = directory / "localhost.pem", directory / "localhost-key.pem"
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(cert, key)
    async def case():
        async with Peer(ssl=context) as peer:
            for url, tls in [(peer.url, None), (peer.url.replace("localhost", "127.0.0.1"), TlsConfig.with_ca((directory / "ca.pem").read_bytes()))]:
                client = rh.AsyncHaystackClient(url, "user", "password", tls=tls)
                with pytest.raises(rh.ClientError):
                    await client.connect()
                await client.close()
                assert client.closed
            assert not peer.requests
    run(case())


def test_cancelled_maximum_scram_derivation_has_bounded_owned_cleanup():
    async def case():
        async with Peer(iterations=1_000_000) as peer:
            client = rh.AsyncHaystackClient(peer.url, "user", "password", timeout=10)
            pending = client.connect()
            await peer.wait_requests(2)
            # Give the independently supplied server-first challenge time to
            # enter the native bounded derivation. The Rust ownership test also
            # uses a deterministic started-work receipt, avoiding a timing oracle.
            await asyncio.sleep(.02)
            pending.cancel()
            with pytest.raises(asyncio.CancelledError):
                await pending
            await client.close()
            assert client.closed
            assert len(peer.requests) == 2  # No proof request after cancellation.
            await peer.wait_eof()
            await peer.wait_idle()
    run(case())


@pytest.mark.parametrize("operation", ["connect", "read"])
@pytest.mark.parametrize("iteration", range(3))
def test_supervised_process_exit_with_unfinished_native_work(operation, iteration):
    import sys
    async def case():
        async with Peer(mode="stall_" + operation) as peer:
            source = '''
import asyncio, sys
import rusty_haystack as rh
client = rh.AsyncHaystackClient(sys.argv[1], "user", "password")
pending = None
async def main():
    global pending
    if sys.argv[2] == "read":
        await client.connect()
        pending = client.read("site")
    else:
        pending = client.connect()
    print("STARTED", flush=True)
    await asyncio.sleep(.1)
asyncio.run(main())
'''
            process = await asyncio.create_subprocess_exec(sys.executable, "-I", "-c", source, peer.url, operation, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
            try:
                stdout, stderr = await asyncio.wait_for(process.communicate(), 5)
            finally:
                if process.returncode is None:
                    process.kill()
                    await process.wait()
            assert process.returncode == 0, stderr.decode()
            assert stdout == b"STARTED\n"
            assert b"Fatal Python error" not in stderr and b"panicked" not in stderr
            await peer.wait_eof()
            await peer.wait_idle()
    run(case())


def test_foreign_thread_cannot_mutate_an_open_owner():
    async def case():
        async with Peer() as peer:
            async with rh.AsyncHaystackClient(peer.url, "user", "password") as client:
                def foreign():
                    async def attempt():
                        for operation in (client.close, client.connect, lambda: client.read("site")):
                            with pytest.raises(RuntimeError, match="different event loop"):
                                operation()
                    asyncio.run(attempt())
                await asyncio.to_thread(foreign)
                assert client.state == "open" and len(peer.requests) == 3
                assert len(await client.read("site")) == 1
    run(case())


def test_close_during_connect_and_immediately_cancelled_future_are_terminal():
    async def case():
        async with Peer(mode="stall_connect") as peer:
            client = rh.AsyncHaystackClient(peer.url, "user", "password")
            pending = client.connect()
            with pytest.raises(rh.ClientError):
                client.connect()
            await peer.wait_requests(1)
            await client.close()
            with pytest.raises(rh.ClientError):
                await pending
            await peer.wait_eof()
            await peer.wait_idle()
        async with Peer() as peer:
            client = rh.AsyncHaystackClient(peer.url, "user", "password")
            pending = client.connect()
            pending.cancel()  # No event-loop turn or bridge poll is assumed.
            with pytest.raises(asyncio.CancelledError):
                await pending
            await client.close()
            assert client.closed
            await peer.wait_idle()
    run(case())


def test_default_budget_expires_and_past_read_deadline_sends_nothing():
    async def case():
        async with Peer(mode="stall_read") as peer:
            client = rh.AsyncHaystackClient(peer.url, "user", "password", timeout=.2)
            await client.connect()
            with pytest.raises(TimeoutError):
                await client.read("site", deadline=asyncio.get_running_loop().time() - 1)
            assert len(peer.requests) == 3
            with pytest.raises(TimeoutError):
                await client.read("site")
            await peer.wait_eof()
            await client.close()
    run(case())


@pytest.mark.parametrize("timeout", [0, -1, float("nan"), float("inf"), 1e300])
def test_invalid_constructor_budget_is_local(timeout):
    with pytest.raises(ValueError):
        rh.AsyncHaystackClient("http://localhost:1", "user", "password", timeout=timeout)


def test_failed_connect_has_retained_terminal_owner_and_sanitized_error():
    async def case():
        async with Peer() as peer:
            client = rh.AsyncHaystackClient(peer.url, "user", "private-password-sentinel")
            with pytest.raises(rh.ClientError) as error:
                await client.connect()
            assert "private-password-sentinel" not in str(error.value)
            await client.close()
            assert client.closed
            with pytest.raises(rh.ClientError):
                client.connect()
            await peer.wait_eof()
            await peer.wait_idle()
    run(case())


def test_past_connect_deadline_never_dispatches_and_closes_owner():
    async def case():
        async with Peer() as peer:
            client = rh.AsyncHaystackClient(peer.url, "user", "password")
            with pytest.raises(TimeoutError):
                await client.connect(deadline=asyncio.get_running_loop().time() - 1)
            await client.close()
            assert client.closed and not peer.requests and not peer.handlers
    run(case())


def _https_peer_context(tmp_path, *, mutual=False):
    import ssl
    directory = tls_material(tmp_path)
    cert, key = directory / "localhost.pem", directory / "localhost-key.pem"
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(cert, key)
    if mutual:
        context.verify_mode = ssl.CERT_REQUIRED
        context.load_verify_locations(cafile=directory / "ca.pem")
    return directory, context


def test_tls_verify_default_and_true_reject_untrusted_before_http(tmp_path):
    """Omitted tls_verify and tls_verify=True must fail before any peer request."""
    from rusty_haystack.client import HaystackClient

    _directory, context = _https_peer_context(tmp_path)

    async def case():
        async with Peer(ssl=context) as peer:
            for kwargs in ({}, {"tls_verify": True}):
                async_client = rh.AsyncHaystackClient(peer.url, "user", "password", **kwargs)
                with pytest.raises(rh.ClientError):
                    await async_client.connect()
                await async_client.close()

                with pytest.raises(rh.ClientError):
                    await asyncio.to_thread(
                        lambda: HaystackClient.connect(peer.url, "user", "password", **kwargs)
                    )
            assert not peer.requests

    run(case())


def test_tls_verify_false_scram_and_authenticated_read(tmp_path):
    """tls_verify=False completes SCRAM and a subsequent authenticated read."""
    from rusty_haystack.client import HaystackClient

    _directory, context = _https_peer_context(tmp_path)

    async def case():
        async with Peer(ssl=context) as peer:
            async with rh.AsyncHaystackClient(
                peer.url, "user", "password", tls_verify=False
            ) as client:
                assert len(await client.read("site")) == 1

            def sync_flow():
                client = HaystackClient.connect(
                    peer.url, "user", "password", tls_verify=False
                )
                try:
                    assert len(client.read("site")) == 1
                finally:
                    client.close()

            await asyncio.to_thread(sync_flow)
            await peer.wait_eof(2)
            await peer.wait_idle()

    run(case())


def test_tls_verify_false_allows_hostname_mismatch(tmp_path):
    """Opt-out disables hostname verification as well as certificate trust."""
    from rusty_haystack.client import HaystackClient

    _directory, context = _https_peer_context(tmp_path)

    async def case():
        async with Peer(ssl=context) as peer:
            mismatched = peer.url.replace("localhost", "127.0.0.1")
            async with rh.AsyncHaystackClient(
                mismatched, "user", "password", tls_verify=False
            ) as client:
                assert len(await client.read("site")) == 1

            def sync_flow():
                client = HaystackClient.connect(
                    mismatched, "user", "password", tls_verify=False
                )
                try:
                    assert len(client.read("site")) == 1
                finally:
                    client.close()

            await asyncio.to_thread(sync_flow)

    run(case())


def test_tls_verify_opt_out_does_not_poison_default_clients(tmp_path):
    """A later default client must still reject the untrusted cert."""
    from rusty_haystack.client import HaystackClient

    _directory, context = _https_peer_context(tmp_path)

    async def case():
        async with Peer(ssl=context) as peer:
            async with rh.AsyncHaystackClient(
                peer.url, "user", "password", tls_verify=False
            ) as insecure:
                assert len(await insecure.read("site")) == 1

            before = len(peer.requests)
            default_async = rh.AsyncHaystackClient(peer.url, "user", "password")
            with pytest.raises(rh.ClientError):
                await default_async.connect()
            await default_async.close()
            assert len(peer.requests) == before

            with pytest.raises(rh.ClientError):
                await asyncio.to_thread(
                    HaystackClient.connect, peer.url, "user", "password"
                )
            assert len(peer.requests) == before

    run(case())


def test_tls_verify_false_still_rejects_bad_credentials(tmp_path):
    """Wrong password fails SCRAM even when TLS verification is disabled."""
    from rusty_haystack.client import HaystackClient

    _directory, context = _https_peer_context(tmp_path)

    async def case():
        async with Peer(ssl=context) as peer:
            bad_async = rh.AsyncHaystackClient(
                peer.url, "user", "wrong-password", tls_verify=False
            )
            with pytest.raises(rh.ClientError):
                await bad_async.connect()
            await bad_async.close()

            with pytest.raises(rh.ClientError):
                await asyncio.to_thread(
                    lambda: HaystackClient.connect(
                        peer.url, "user", "wrong-password", tls_verify=False
                    )
                )

            # No authenticated read: only HELLO/SCRAM attempts, never /api/read.
            assert peer.requests
            assert all(path != "/api/read" for _, path, _, _ in peer.requests)

    run(case())


def test_async_tls_identity_retained_with_tls_verify_false(tmp_path):
    """Async client keeps client cert/key when tls= is combined with tls_verify=False."""
    from rusty_haystack.client import TlsConfig

    directory, context = _https_peer_context(tmp_path, mutual=True)
    cert, key = directory / "localhost.pem", directory / "localhost-key.pem"
    tls = TlsConfig.from_files(str(cert), str(key), str(directory / "ca.pem"))

    async def case():
        async with Peer(ssl=context) as peer:
            async with rh.AsyncHaystackClient(
                peer.url, "user", "password", tls=tls, tls_verify=False
            ) as client:
                assert len(await client.read("site")) == 1
            await peer.wait_eof()
            await peer.wait_idle()

    run(case())
