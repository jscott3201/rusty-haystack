"""Controllable embedded applications, observed through real asyncio and sockets."""
import asyncio
import socket

import pytest
import rusty_haystack as rh

from .application_fixture import auth_manager, cancel_entry_after_completion, ids, request


def run(coroutine):
    async def bounded():
        async with asyncio.timeout(15):
            await coroutine
    return asyncio.run(bounded())


def graph():
    return rh.SharedGraph.from_grid(rh.decode_grid("text/zinc", 'ver:"3.0"\nid,site,dis\n@a,M,"Alpha"\n@b,M,"Beta"\n'))


def test_builder_requires_loop_and_consumes_configuration_once():
    builder = rh.ApplicationBuilder(graph())
    with pytest.raises(RuntimeError):
        builder.start()
    async def case():
        app = builder.start()
        with pytest.raises(RuntimeError, match="consumed"):
            builder.start()
        assert isinstance(app, rh.ApplicationOwner)
        address = await app.ready()
        assert address[0] == "127.0.0.1" and address[1] > 0
        await app.close()
        assert app.closed and app.state == "closed"
    run(case())


def test_embedded_pagination_and_borrowed_handle_graph_runtime():
    borrowed = graph()
    async def case():
        async with rh.ApplicationBuilder(borrowed).start() as app:
            handle = app.handle()
            assert not hasattr(handle, "close")
            page = await handle.read("site", limit=1)
            assert len(page) == 1 and not page.meta()["complete"]
            cursor = page.meta()["cursor"]
            final = await app.read("site", limit=1, cursor=cursor)
            assert len(final) == 1 and final.meta()["complete"]
        assert handle.closed
        assert (await handle.terminated()).close_error is None
        with pytest.raises(rh.ApplicationError):
            handle.read("site")
        assert len(borrowed.read("site")) == 2
        async with rh.ApplicationBuilder(borrowed).start() as other:
            assert len(await other.read("site")) == 2
    run(case())


def test_eager_close_and_cancelled_observer_are_terminal():
    async def case():
        app = rh.ApplicationBuilder(graph()).start()
        await app.ready()
        close = app.close()
        with pytest.raises(rh.ApplicationError):
            app.read("site")
        close.cancel()
        await asyncio.gather(close, return_exceptions=True)
        await asyncio.gather(app.close(), app.close())
        assert app.closed
    run(case())


@pytest.mark.parametrize("observation", ["ready", "enter"])
def test_bind_failure_preserves_failed_termination_and_borrowed_socket(observation):
    with socket.socket() as occupied:
        occupied.bind(("127.0.0.1", 0))
        occupied.listen()
        async def case():
            app = rh.ApplicationBuilder(graph(), port=occupied.getsockname()[1]).start()
            with pytest.raises(rh.ApplicationError, match="bind"):
                if observation == "ready":
                    await app.ready()
                else:
                    async with app:
                        raise AssertionError("failed startup entered its context body")
            report = await app.terminated()
            assert report.close_error and app.closed and app.state == "failed"
            with pytest.raises(rh.ApplicationError):
                await app.close()
            assert occupied.getsockname()[1] > 0
        run(case())


def test_accepted_socket_teardown_and_exact_port_reuse_before_peer_teardown():
    async def case():
        app = rh.ApplicationBuilder(graph()).start()
        address = await app.ready()
        reader, writer = await asyncio.open_connection(*address)
        writer.write(b"GET /api/ops HTTP/1.1\r\nHost: localhost\r\n\r\n")
        await writer.drain()
        assert b"200" in await reader.readuntil(b"\r\n\r\n")
        writer.write(b"POST /api/read HTTP/1.1\r\nHost: localhost\r\nContent-Type: text/zinc\r\nContent-Length: 999\r\n\r\nver:\"3.0\"\n")
        await writer.drain()
        # The first response proves accept registered; retain this peer until EOF.
        await app.close()
        try:
            while await reader.read(4096):
                pass
        except ConnectionResetError:
            # Closing with unread partial request bytes can produce TCP reset.
            pass
        with socket.socket() as rebound:
            rebound.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            rebound.bind(address)
            rebound.listen()
        writer.close()
        try:
            await writer.wait_closed()
        except ConnectionResetError:
            pass
    run(case())


def test_immediate_close_races_start_and_empty_owned_graph():
    async def case():
        for _ in range(5):
            app = rh.ApplicationBuilder().start()
            await app.close()
            assert app.closed
            with pytest.raises(rh.ApplicationError):
                await app.ready()
        async with rh.ApplicationBuilder().start() as empty:
            assert len(await empty.read("site")) == 0
    run(case())


def test_http_scram_matches_trusted_reads_and_denies_other_principals():
    borrowed = graph()
    async def case():
        async with rh.ApplicationBuilder(borrowed, auth=auth_manager()).start() as app:
            address = await app.ready()
            url = f"http://{address[0]}:{address[1]}/api"
            status, _ = await asyncio.to_thread(request, address, "/api/read", 'ver:"3.0"\nfilter\n"site"\n')
            assert status == 401
            async with rh.AsyncHaystackClient(url, "reader", "password") as client:
                assert ids(await client.read("site")) == ids(await app.read("site")) == ["a", "b"]
            async with rh.AsyncHaystackClient(url, "denied", "password") as client:
                with pytest.raises(rh.ClientError):
                    await client.read("site")
            invalid = rh.AsyncHaystackClient(url, "reader", "wrong-password")
            with pytest.raises(rh.ClientError):
                await invalid.connect()
            await invalid.close()
            # Authenticated admin still cannot select unsupported routes.
            def check_capabilities():
                client = rh.client.HaystackClient.connect(url, "admin", "password")
                try:
                    names = {row["name"] for row in client.ops()}
                    assert {"read", "nav", "defs", "libs"} <= names
                    assert not names & {"hisRead", "hisWrite", "watchSub", "ws", "entityBatch", "pointWrite"}
                    for operation in ("hisWrite", "watchSub", "entityBatch", "pointWrite"):
                        with pytest.raises(rh.ClientError):
                            client.call(operation, rh.HGrid())
                finally:
                    client.close()
            await asyncio.to_thread(check_capabilities)
            assert ids(await app.read("site")) == ["a", "b"]
        assert len(borrowed) == 2
    run(case())


def test_without_auth_http_entity_data_is_denied_but_embedding_works():
    async def case():
        async with rh.ApplicationBuilder(graph()).start() as app:
            address = await app.ready()
            status, body = await asyncio.to_thread(request, address, "/api/read", 'ver:"3.0"\nfilter\n"site"\n')
            assert status == 403 and b"Alpha" not in body
            assert ids(await app.read("site")) == ["a", "b"]
    run(case())


def test_invalid_start_and_constructor_leave_auth_available():
    auth = auth_manager()
    with pytest.raises(ValueError):
        rh.ApplicationBuilder(host="localhost", auth=auth)
    builder = rh.ApplicationBuilder(auth=auth)
    with pytest.raises(RuntimeError):
        builder.start()
    assert auth.is_enabled()
    async def case():
        app = builder.start()
        with pytest.raises(RuntimeError):
            auth.is_enabled()
        await app.close()
    run(case())


def test_cancelled_ready_observer_keeps_owner_but_cancelled_enter_closes():
    async def case():
        app = rh.ApplicationBuilder(graph()).start()
        ready = app.ready()
        ready.cancel()
        await asyncio.gather(ready, return_exceptions=True)
        await app.ready()
        assert ids(await app.read("site")) == ["a", "b"]
        await app.close()
        for _ in range(5):
            app = rh.ApplicationBuilder(graph()).start()
            entering = asyncio.ensure_future(app.__aenter__())
            entering.cancel()
            await asyncio.gather(entering, return_exceptions=True)
            await app.terminated()
            assert app.closed
    run(case())


def test_drop_owner_closes_even_when_borrowed_handle_remains():
    async def case():
        import gc
        app = rh.ApplicationBuilder(graph()).start()
        await app.ready()
        handle = app.handle()
        del app
        gc.collect()
        await handle.terminated()
        assert handle.closed
        with pytest.raises(rh.ApplicationError):
            handle.read("site")
    run(case())


@pytest.mark.parametrize("catch_cancellation", [False, True])
def test_context_entry_cancelled_after_future_completion(catch_cancellation):
    async def case():
        app = rh.ApplicationBuilder(graph()).start()
        address = await app.ready()
        try:
            await cancel_entry_after_completion(app, catch_cancellation=catch_cancellation)
            with socket.socket() as rebound:
                rebound.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                rebound.bind(address)
                rebound.listen()
        finally:
            await app.close()
    run(case())


@pytest.mark.parametrize("disposal", ["close", "cancel_task", "drop"])
def test_unstarted_context_entry_disposal_requests_shutdown(disposal):
    async def case():
        app = rh.ApplicationBuilder(graph()).start()
        address = await app.ready()
        try:
            entering = app.__aenter__()
            assert asyncio.iscoroutine(entering)
            if disposal == "close":
                entering.close()
            elif disposal == "cancel_task":
                task = asyncio.create_task(entering)
                task.cancel()  # No task turn has polled the entry coroutine.
                with pytest.raises(asyncio.CancelledError):
                    await task
            else:
                del entering
            # Retain the owner in every case and the entry object except in the
            # explicit drop case; cleanup must not depend on eventual collection.
            assert app.state in ("closing", "closed")
            await asyncio.wait_for(app.terminated(), 2)
            assert app.closed
            with socket.socket() as rebound:
                rebound.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                rebound.bind(address)
                rebound.listen()
        finally:
            await app.close()
    run(case())


def test_deadlines_validation_cancellation_and_reusable_capacity():
    async def case():
        app = rh.ApplicationBuilder(graph()).start()
        await app.ready()
        loop = asyncio.get_running_loop()
        for operation in (app.ready, app.close, app.terminated):
            with pytest.raises(ValueError):
                operation(deadline=float("nan"))
        assert app.state == "running"
        for limit in (0, 1001, -1):
            with pytest.raises((ValueError, OverflowError)):
                app.read("site", limit=limit)
        with pytest.raises(ValueError):
            app.read("é" * 4097)
        with pytest.raises(TimeoutError):
            app.read("site", deadline=loop.time() - 1)
        with pytest.raises(TimeoutError):
            await app.terminated(deadline=loop.time() + .01)
        # Capacity saturation is tested with explicitly held native permits.
        # These conversions may finish concurrently on free-threaded CPython.
        reads = [app.read("site") for _ in range(16)]
        for read in reads:
            read.cancel()
        await asyncio.gather(*reads, return_exceptions=True)
        for _ in range(100):
            try:
                resumed = app.read("site")
                break
            except rh.ApplicationError:
                await asyncio.sleep(.01)
        else:
            raise AssertionError("cancelled read guards did not drain")
        assert len(await resumed) == 2
        closing = app.close(deadline=loop.time() - 1)
        with pytest.raises(rh.ApplicationError):
            app.read("site")
        with pytest.raises(TimeoutError):
            await closing
        await app.close()
        assert (await app.terminated(deadline=loop.time() - 1)).close_error is None
    run(case())


def test_affinity_context_future_patterns_and_original_exception():
    import contextvars
    import threading
    marker = contextvars.ContextVar("embedding-context")
    retained = []
    async def case():
        app = rh.ApplicationBuilder(graph()).start()
        retained.append(app)
        async with app:
            async def read(value):
                marker.set(value)
                result = app.read("site")
                assert isinstance(result, asyncio.Future)
                assert len(await asyncio.ensure_future(result)) == 2
                assert marker.get() == value
            async with asyncio.TaskGroup() as tasks:
                tasks.create_task(read("one"))
                tasks.create_task(read("two"))
            errors = []
            def foreign():
                async def wrong_loop():
                    for method in (app.ready, app.close, app.terminated, lambda: app.handle().read("site")):
                        try:
                            method()
                        except RuntimeError:
                            errors.append(True)
                asyncio.run(wrong_loop())
            thread = threading.Thread(target=foreign)
            thread.start()
            await asyncio.to_thread(thread.join, 3)
            assert not thread.is_alive() and len(errors) == 4
            assert app.state == "running"
        with pytest.raises(ValueError, match="body sentinel"):
            async with rh.ApplicationBuilder().start():
                raise ValueError("body sentinel")
    run(case())
    async def later_loop():
        with pytest.raises(RuntimeError, match="different event loop"):
            retained[0].close()
    run(later_loop())


@pytest.mark.parametrize("stage", ["starting", "running", "reading"])
def test_supervised_unfinished_application_exit(stage):
    import subprocess
    import sys
    code = f'''
import asyncio
import rusty_haystack as rh
retained = []
async def main():
    app = rh.ApplicationBuilder().start()
    retained.append(app)
    if {stage!r} != "starting":
        await app.ready()
    if {stage!r} == "reading":
        retained.append(app.read("site"))
asyncio.run(main())
'''
    result = subprocess.run([sys.executable, "-I", "-c", code], capture_output=True, text=True, timeout=8)
    assert result.returncode == 0, result.stderr
    assert "panicked" not in result.stderr and "Fatal Python error" not in result.stderr
    assert result.stderr == "", result.stderr
