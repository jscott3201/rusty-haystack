"""Python boundary evidence against the real first-party Rust server."""
import asyncio
import pytest
import rusty_haystack as rh
from .watch_fixture import Fixture, build_fixture

@pytest.fixture(scope="module")
def watch_binary():
    return build_fixture()

def run(work):
    async def bounded():
        async with asyncio.timeout(20):
            return await work
    return asyncio.run(bounded())

async def client(fixture):
    value = rh.AsyncHaystackClient(fixture.url, "user", "pencil", timeout=3)
    await value.connect()
    return value

@pytest.mark.parametrize("direct", [True, False])
def test_initial_values_copies_explicit_ack_changes_and_child_cleanup(watch_binary, direct):
    async def exercise(f):
        parent = await client(f)
        sibling = parent.watch(["parent"], key="sibling")
        assert (await sibling.ready()).kind == "ready"
        watch = parent.watch(["a", "b"], key="known")
        assert watch.state == "new"
        async with watch as entered:
            assert entered is watch and watch.__aiter__() is watch
            first = await anext(watch)
            assert isinstance(first, rh.WatchDelivery)
            assert first.initial and first.removed == () and first.from_revision == first.through
            assert {row.ref_val for row in first.changed} == {"a", "b"}
            a = next(row for row in first.changed if row.ref_val == "a")
            assert a["exact"] == rh.Int(2**63-1)
            assert a["bits"] == rh.Float.from_bits(0x7ff8000000000042)
            assert a["buf"] == rh.Buf(b"\x00\xff")
            a.set("value", 999)
            a["nested"][0].set("value", 999)
            fresh = next(row for row in first.changed if row.ref_val == "a")
            assert fresh["value"] == rh.Number(1) and fresh["nested"][0]["value"] == rh.Number(7)
            with pytest.raises(AttributeError):
                first.token = "bad"
            with pytest.raises(rh.ClientError, match="acknowledge"):
                watch.next()
            ack = await watch.ack(first)
            assert ack.kind == "acknowledged" and ack.token == first.token and ack.through == first.through
            f.command("update", id="a", value=5)
            f.command("remove", id="b")
            second = await anext(watch)
            assert isinstance(second, rh.WatchDelivery)
            assert not second.initial and second.from_revision == first.through and second.through > first.through
            assert second.token != first.token and second.watch_id == first.watch_id
            assert second.removed == ("b",) and len(second.changed) == 1 and second.changed[0]["value"] == rh.Number(5)
            assert second.changed[0]["exact"] == rh.Int(2**63-1)
            await watch.ack(second)
        report = await watch.aclose()
        assert report.kind == "closed" and report.local_joined and watch.closed
        stats = await f.wait(lambda s: s["active"] == 1)
        assert stats["bindings"] == 2
        assert len(await parent.read('id==@parent')) == 1
        assert isinstance(await anext(sibling), rh.WatchDelivery)
        await parent.close()
        assert sibling.closed
        await f.wait(lambda s: s["bindings"] == 0)
    with Fixture(watch_binary, direct=direct) as f:
        run(exercise(f))

@pytest.mark.parametrize("schedule", ["committed_loss", "not_committed", "attach_failure"])
def test_owned_create_cleanup_keeps_exact_intent_and_sibling(watch_binary, schedule):
    async def exercise(f):
        parent = await client(f)
        sibling = parent.watch(["parent"], key="sibling")
        await sibling.ready()
        before = f.command("stats")
        options = {"committed_loss": {"hold_create": True, "lose_create": 1}, "not_committed": {"skip_create": 1}, "attach_failure": {"reject_ws": True}}[schedule]
        f.command("set", **options)
        watch = parent.watch(["b", "a", "a"], key="cleanup", lease_ms=5000)
        ready = watch.ready()
        if schedule == "committed_loss":
            await f.wait(lambda s: s["committed"] == before["committed"] + 1)
            ready.cancel()
            with pytest.raises(asyncio.CancelledError):
                await ready
            closing = watch.aclose()
            closing.cancel()
            with pytest.raises(asyncio.CancelledError):
                await closing
            f.command("set", hold_create=False)
        else:
            assert (await ready).kind == "unknown"
        report = await watch.aclose()
        assert report.kind == "closed" and report.local_joined and watch.closed
        stats = await f.wait(lambda s: s["active"] == 1 and s["ws_active"] == 1)
        child = [x for x in stats["log"] if x["key"] == "cleanup"]
        assert [x["op"] for x in child] == (["watchSub", "watchUnsub"] if schedule == "attach_failure" else ["watchSub", "watchSub", "watchUnsub"])
        creates = [x for x in child if x["op"] == "watchSub"]
        assert len({x["request"] for x in creates}) == 1
        assert len({x["session"] for x in child}) == 1
        assert stats["bindings"] == 2
        f.command("set", reject_ws=False)
        assert len(await parent.read('id==@parent')) == 1
        delivery = await anext(sibling)
        assert (await sibling.ack(delivery)).kind == "acknowledged"
        await parent.close()
        await f.wait(lambda s: s["bindings"] == 0 and s["ws_active"] == 0)
    with Fixture(watch_binary) as f:
        run(exercise(f))


def test_lost_ack_explicit_same_session_reconciliation_and_no_hidden_dispatch(watch_binary):
    async def exercise(f):
        parent = await client(f)
        watch = parent.watch(["a"], key="ack")
        await watch.ready()
        delivery = await anext(watch)
        before = f.command("stats")
        with pytest.raises(rh.ClientError, match="acknowledge"):
            anext(watch)
        assert f.command("stats")["log"] == before["log"]
        f.command("set", lose_ack=1)
        result = await watch.ack(delivery)
        assert result.kind == "unknown" and watch.pending.token == delivery.token
        with pytest.raises(rh.ClientError):
            watch.next()
        result = await watch.resume()
        assert result.kind == "acknowledged" and result.token == delivery.token
        assert watch.pending is None
        calls = [x for x in f.command("stats")["log"] if x["key"] == "ack"]
        assert [x["op"] for x in calls] == ["watchSub", "watchAck", "watchAck"]
        assert calls[1]["request"] == calls[2]["request"]
        assert len({x["session"] for x in calls}) == 1
        f.command("update", id="a", value=12)
        following = await anext(watch)
        assert following.from_revision == delivery.through and following.changed[0]["value"] == rh.Number(12)
        await watch.aclose()
        await parent.close()
    with Fixture(watch_binary) as f:
        run(exercise(f))

@pytest.mark.parametrize("reason", ["gap", "overflow", "lease_expired"])
def test_terminal_controls_require_explicit_new_observation(watch_binary, reason):
    async def exercise(f):
        parent = await client(f)
        watch = parent.watch(["a"], key=reason, lease_ms=150 if reason == "lease_expired" else 5000)
        await watch.ready()
        initial = await anext(watch)
        await watch.ack(initial)
        if reason == "gap":
            for n in range(8):
                f.command("update", id="parent", value=n)
        elif reason == "overflow":
            f.command("overflow")
        else:
            await f.wait(lambda s: s["active"] == 0)
        control = await anext(watch)
        assert isinstance(control, rh.WatchControl) and control.kind == "resync" and control.reason == reason
        with pytest.raises(StopAsyncIteration):
            await anext(watch)
        assert not any(x["op"] == "watchRenew" for x in f.command("stats")["log"])
        await watch.aclose()
        fresh = parent.watch(["parent"], key=reason + "-fresh")
        assert isinstance(await anext(fresh), rh.WatchDelivery)
        await parent.close()
    with Fixture(watch_binary) as f:
        run(exercise(f))


def test_pending_cleanup_attempt_cap_is_unknown_and_preserves_other_watch(watch_binary):
    async def exercise(f):
        parent = await client(f)
        sibling = parent.watch(["parent"], key="sibling")
        await sibling.ready()
        f.command("set", prepare_loss=True)
        child = parent.watch(["a"], key="preparing", lease_ms=300)
        assert (await child.ready()).kind == "unknown"
        stats = f.command("stats")
        assert stats["gate_entered"] and stats["bindings"] == 2 and stats["committed"] == 1
        report = await child.aclose()
        assert report.kind == "unknown" and report.local_joined and child.closed
        stats = f.command("stats")
        calls = [x for x in stats["log"] if x["key"] == "preparing"]
        assert len(calls) == 6 and {x["op"] for x in calls} == {"watchSub"}
        assert len({x["request"] for x in calls}) == 1
        f.command("release")
        await f.wait(lambda s: s["active"] == 1)
        assert len(await parent.read('id==@parent')) == 1
        await parent.close()
    with Fixture(watch_binary) as f:
        run(exercise(f))


def test_unconfirmed_unsubscribe_is_distinct_from_local_join_and_expires(watch_binary):
    async def exercise(f):
        parent = await client(f)
        child = parent.watch(["a"], key="unreachable", lease_ms=500)
        await child.ready()
        f.command("set", skip_unsub=100)
        closing = child.aclose(deadline=asyncio.get_running_loop().time() - 1)
        with pytest.raises(TimeoutError):
            await closing
        with pytest.raises(rh.ClientError):
            child.next()
        report = await child.aclose()
        assert report.kind == "unknown" and report.local_joined and child.closed
        stats = f.command("stats")
        assert stats["active"] == 1
        assert len([x for x in stats["log"] if x["op"] == "watchUnsub"]) == 6
        await f.wait(lambda s: s["active"] == 0)
        await parent.close()
    with Fixture(watch_binary) as f:
        run(exercise(f))


def test_cancelled_receive_overlap_and_explicit_resume_renew(watch_binary):
    async def exercise(f):
        parent = await client(f)
        watch = parent.watch(["a"], key="receive")
        initial = await anext(watch)
        await watch.ack(initial)
        f.command("update", id="a", value=15)
        f.command("set", hold_poll=True)
        receiving = watch.next()
        assert isinstance(receiving, asyncio.Future)
        stats = await f.wait(lambda s: any(x["op"] == "watchPoll" for x in s["log"]))
        for operation in (watch.next, watch.ready, watch.resume, lambda: watch.renew(5000)):
            with pytest.raises(rh.ClientError, match="overlaps"):
                operation()
        assert f.command("stats")["log"] == stats["log"]
        # Real unrelated HTTP I/O completes while this WS result is held.
        assert len(await parent.read('id==@parent')) == 1
        receiving.cancel()
        with pytest.raises(asyncio.CancelledError):
            await receiving
        await f.wait(lambda _: watch.state == "uncertain")
        with pytest.raises(rh.ClientError, match="uncertain"):
            watch.ready()
        with pytest.raises(rh.ClientError, match="uncertain"):
            watch.next()
        f.command("set", hold_poll=False)
        replay = await watch.resume()
        assert isinstance(replay, rh.WatchDelivery) and replay.changed[0]["value"] == rh.Number(15)
        receipts = [x for x in f.command("stats")["deliveries"] if x["key"] == "receive"]
        assert receipts[-1] == receipts[-2], "resume must repeat the already prepared payload"
        assert (await watch.ack(replay)).kind == "acknowledged"
        assert (await watch.renew(5000)).kind == "renewed"
        calls = [x for x in f.command("stats")["log"] if x["key"] == "receive"]
        assert [x["op"] for x in calls] == ["watchSub", "watchAck", "watchPoll", "watchPoll", "watchAck", "watchRenew"]
        assert len({x["session"] for x in calls}) == 1
        await parent.close()
        assert watch.closed
    with Fixture(watch_binary) as f:
        run(exercise(f))


def test_entry_before_poll_and_completed_future_handoff_close_owned_child(watch_binary):
    from .watch_fixture import cancel_entry_after_completion
    async def exercise(f):
        parent = await client(f)
        baseline = f.command("stats")["log"]
        for mode in ("close", "throw", "task_cancel", "past_deadline"):
            watch = parent.watch(["a"], key=mode)
            if mode == "past_deadline":
                with pytest.raises(TimeoutError):
                    await watch.ready(deadline=asyncio.get_running_loop().time() - 1)
            else:
                entry = watch.__aenter__()
                if mode == "close":
                    entry.close()
                elif mode == "throw":
                    with pytest.raises(asyncio.CancelledError):
                        entry.throw(asyncio.CancelledError())
                else:
                    task = asyncio.create_task(entry)
                    task.cancel()
                    with pytest.raises(asyncio.CancelledError):
                        await task
            report = await watch.aclose()
            assert report.kind == "not_created" and report.local_joined
        assert f.command("stats")["log"] == baseline
        retained = parent.watch(["a"], key="handoff")
        await cancel_entry_after_completion(retained)
        assert f.command("stats")["active"] == 0
        assert len(await parent.read('id==@parent')) == 1
        await parent.close()
    with Fixture(watch_binary) as f:
        run(exercise(f))


@pytest.mark.parametrize("body_error", [False, True])
def test_context_cleanup_failure_is_observable_without_masking_body(watch_binary, body_error):
    async def exercise(f):
        parent = await client(f)
        watch = parent.watch(["a"], key="exit", lease_ms=1000)
        error = ValueError("application body")
        try:
            async with watch:
                f.command("set", skip_unsub=100)
                if body_error:
                    raise error
        except ValueError as caught:
            assert body_error and caught is error
            assert any("cleanup" in note.lower() for note in error.__notes__)
        except rh.ClientError as caught:
            assert not body_error and "cleanup is unconfirmed" in str(caught)
        else:
            raise AssertionError("context exit hid unconfirmed remote cleanup")
        report = await watch.aclose()
        assert report.kind == "unknown" and report.local_joined and watch.closed
        assert f.command("stats")["active"] == 1
        await parent.close()
        await f.wait(lambda s: s["active"] == 0 and s["bindings"] == 0)
    with Fixture(watch_binary) as f:
        run(exercise(f))


def test_loop_thread_context_future_and_retained_parent_close(watch_binary):
    import contextvars
    import threading
    var = contextvars.ContextVar("watch-caller")
    retained = []
    async def exercise(f):
        parent = await client(f)
        watch = parent.watch(["a"], key="context")
        retained.append(watch)
        var.set("owner-context")
        seen = []
        future = watch.ready()
        assert isinstance(future, asyncio.Future)
        future.add_done_callback(lambda _: seen.append((var.get(), threading.get_ident())))
        assert (await asyncio.ensure_future(future)).kind == "ready"
        await asyncio.sleep(0)
        assert seen == [("owner-context", threading.get_ident())]
        async def wrong_thread():
            with pytest.raises(RuntimeError, match="different event loop or OS thread"):
                watch.next()
        await asyncio.to_thread(lambda: asyncio.run(wrong_thread()))
        async def receive():
            return await anext(watch)
        async with asyncio.TaskGroup() as group:
            task = group.create_task(receive())
        delivery = task.result()
        with pytest.raises(AttributeError):
            delivery.scope_generation = 100
        assert (await watch.ack(delivery)).kind == "acknowledged"
        if hasattr(asyncio, "eager_task_factory"):
            loop = asyncio.get_running_loop()
            old = loop.get_task_factory()
            loop.set_task_factory(asyncio.eager_task_factory)
            try:
                other = parent.watch(["b"], key="eager")
                entered = await asyncio.create_task(other.__aenter__())
                assert entered is other
            finally:
                loop.set_task_factory(old)
        # Both children and their retained payloads remain reachable during parent close.
        await parent.close()
        assert parent.closed and watch.closed and (await watch.aclose()).local_joined
        if hasattr(asyncio, "eager_task_factory"):
            assert other.closed
        await f.wait(lambda s: s["active"] == 0 and s["bindings"] == 0 and s["ws_active"] == 0)
    with Fixture(watch_binary) as f:
        run(exercise(f))
    async def wrong_loop():
        with pytest.raises(RuntimeError, match="different event loop or OS thread"):
            retained[0].aclose()
    run(wrong_loop())


def test_forbidden_scope_and_foreign_ack_reject_without_advancement(watch_binary):
    async def exercise(f):
        parent = await client(f)
        denied = parent.watch(["denied"], key="denied")
        report = await denied.ready()
        assert report.kind == "rejected" and report.reason == "forbidden"
        first, second = parent.watch(["a"], key="first"), parent.watch(["b"], key="second")
        a, b = await anext(first), await anext(second)
        before = f.command("stats")["log"]
        with pytest.raises(rh.ClientError, match="does not match"):
            first.ack(b)
        assert f.command("stats")["log"] == before
        assert (await first.ack(a)).kind == "acknowledged"
        await parent.close()
    with Fixture(watch_binary) as f:
        run(exercise(f))


@pytest.mark.parametrize("stage", ["create", "receive", "explicit"])
def test_supervised_closed_loop_and_normal_exit(watch_binary, stage):
    import sys
    source = '''
import asyncio, sys
import rusty_haystack as rh
parent = rh.AsyncHaystackClient(sys.argv[1], "user", "pencil", timeout=2)
watch = pending = None
async def main():
    global watch, pending
    await parent.connect()
    watch = parent.watch(["a"], key="process", lease_ms=500, cleanup_timeout=.4)
    if sys.argv[2] == "create":
        pending = watch.ready()
    elif sys.argv[2] == "receive":
        delivery = await anext(watch)
        await watch.ack(delivery)
        pending = watch.next()
    else:
        delivery = await anext(watch)
        await parent.close()
        assert watch.closed and parent.closed
loop = asyncio.new_event_loop()
loop.run_until_complete(main())
loop.close()
print("LOOP_CLOSED", flush=True)
sys.stdin.readline()
print("EXIT", flush=True)
'''
    async def exercise(f):
        if stage != "explicit":
            f.command("set", **({"hold_create": True} if stage == "create" else {"hold_poll": True}))
        process = await asyncio.create_subprocess_exec(sys.executable, "-I", "-c", source, f.url, stage, stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
        try:
            assert await asyncio.wait_for(process.stdout.readline(), 5) == b"LOOP_CLOSED\n"
            if stage == "create":
                await f.wait(lambda s: s["committed"] == 1)
            elif stage == "receive":
                await f.wait(lambda s: any(x["op"] == "watchPoll" for x in s["log"]))
            else:
                assert f.command("stats")["active"] == 0
                assert any(x["op"] == "close" for x in f.command("stats")["log"])
                # Session invalidation is synchronous; the existing service's
                # maintenance tick reclaims its bounded tombstone bindings.
                await f.wait(lambda s: s["bindings"] == 0)
            f.command("set", hold_create=False, hold_poll=False)
            stdout, stderr = await asyncio.wait_for(process.communicate(b"exit\n"), 5)
            assert process.returncode == 0 and stdout == b"EXIT\n", stderr.decode()
            assert not any(x in stderr for x in (b"Fatal Python error", b"panicked", b"Event loop is closed")), stderr.decode()
            # In unawaited exit cases, server lease expiry is independent of
            # interpreter completion; only explicit close proves graceful join.
            await f.wait(lambda s: s["active"] == 0 and s["ws_active"] == 0)
        finally:
            if process.returncode is None:
                process.kill()
                await process.wait()
    with Fixture(watch_binary) as f:
        run(exercise(f))


def test_invalid_intent_and_child_capacity_are_local(watch_binary):
    async def exercise(f):
        parent = await client(f)
        baseline = f.command("stats")["log"]
        for options in ({"ids": [], "key": "empty"}, {"ids": ["a"], "key": ""}, {"ids": ["a"], "key": "x" * 129}, {"ids": ["a"], "key": "lease", "lease_ms": 0}, {"ids": ["a"], "key": "lease", "lease_ms": 3_600_001}, {"ids": ["a"], "key": "cleanup", "cleanup_timeout": float("nan")}, {"ids": ["a"], "key": "cleanup", "cleanup_timeout": 31}):
            with pytest.raises((rh.ClientError, ValueError)):
                parent.watch(**options)
        children = []
        for index in range(32):
            children.append(parent.watch(["a"], key=f"capacity-{index}"))
            # A completed parent read is a native command-queue barrier. An
            # asyncio sleep(0) alone does not synchronize its owner OS thread.
            if index % 8 == 7:
                assert len(await parent.read('id==@parent')) == 1
        with pytest.raises(rh.ClientError):
            parent.watch(["a"], key="capacity-overflow")
        assert not any(x["op"].startswith("watch") for x in f.command("stats")["log"])
        assert f.command("stats")["log"][:len(baseline)] == baseline
        assert (await children.pop().aclose()).kind == "not_created"
        replacement = parent.watch(["a"], key="capacity-reused")
        await parent.close()
        assert replacement.closed and all(child.closed for child in children)
    with Fixture(watch_binary) as f:
        run(exercise(f))


def test_cleanup_deadline_exhaustion_does_not_start_reconciliation(watch_binary):
    async def exercise(f):
        parent = await client(f)
        f.command("set", hold_create=True)
        watch = parent.watch(["a"], key="cleanup-deadline", lease_ms=500, cleanup_timeout=.05)
        original = watch.ready()
        await f.wait(lambda s: s["committed"] == 1)
        report = await watch.aclose()
        assert report.kind == "unknown" and report.local_joined and watch.closed
        assert (await original).kind == "unknown"
        stats = f.command("stats")
        calls = [x for x in stats["log"] if x["key"] == "cleanup-deadline"]
        assert [x["op"] for x in calls] == ["watchSub"]
        assert stats["active"] == 1
        f.command("set", hold_create=False)
        assert len(await parent.read('id==@parent')) == 1
        await f.wait(lambda s: s["active"] == 0)
        assert [x for x in f.command("stats")["log"] if x["key"] == "cleanup-deadline"] == calls
        await parent.close()
    with Fixture(watch_binary) as f:
        run(exercise(f))


@pytest.mark.parametrize("acknowledged", [False, True])
def test_creation_key_is_reserved_before_alias_registration(watch_binary, acknowledged):
    async def exercise(f):
        parent = await client(f)
        sibling = parent.watch(["parent"], key="key-sibling")
        sibling_delivery = await anext(sibling)
        owned = parent.watch(["b", "a", "a"], key="owned-key", lease_ms=5000)
        first = await anext(owned)
        if acknowledged:
            await owned.ack(first)
        before = f.command("stats")["log"]
        for ids in (["b", "a", "a"], ["a"]):
            with pytest.raises(rh.ClientError, match="key"):
                parent.watch(ids, key="owned-key", lease_ms=5000)
        assert f.command("stats")["log"] == before
        if not acknowledged:
            assert (await owned.ack(first)).kind == "acknowledged"
        f.command("update", id="a", value=31)
        changed = await anext(owned)
        assert changed.watch_id == first.watch_id and changed.changed[0]["value"] == rh.Number(31)
        assert (await owned.aclose()).kind == "closed"
        assert (await sibling.ack(sibling_delivery)).kind == "acknowledged"
        assert f.command("stats")["active"] == 1
        assert len(await parent.read('id==@parent')) == 1
        await parent.close()
    with Fixture(watch_binary) as f:
        run(exercise(f))


def test_unknown_cleanup_reserves_key_after_handle_is_dropped(watch_binary):
    import gc
    async def exercise(f):
        parent = await client(f)
        sibling = parent.watch(["parent"], key="unknown-sibling")
        other = await anext(sibling)
        owned = parent.watch(["a"], key="unknown-owned", lease_ms=5000)
        initial = await anext(owned)
        await owned.ack(initial)
        f.command("set", skip_unsub=100)
        report = await owned.aclose()
        assert report.kind == "unknown" and report.local_joined and owned.closed
        del owned
        gc.collect()
        before = f.command("stats")
        assert before["active"] == 2
        with pytest.raises(rh.ClientError, match="key"):
            parent.watch(["a"], key="unknown-owned", lease_ms=5000)
        assert f.command("stats")["log"] == before["log"]
        f.command("set", skip_unsub=0)
        distinct = parent.watch(["b"], key="distinct")
        assert isinstance(await anext(distinct), rh.WatchDelivery)
        assert (await distinct.aclose()).kind == "closed"
        assert (await sibling.ack(other)).kind == "acknowledged"
        assert len(await parent.read('id==@parent')) == 1
        assert f.command("stats")["active"] == 2
        await parent.close()
        await f.wait(lambda s: s["active"] == 0 and s["bindings"] == 0)
    with Fixture(watch_binary) as f:
        run(exercise(f))


@pytest.mark.parametrize("skipped", [6, 5])
@pytest.mark.parametrize("idle", [False, True])
def test_cleanup_reserves_last_request_for_unsubscribe(watch_binary, skipped, idle):
    async def exercise(f):
        parent = await client(f)
        sibling = parent.watch(["parent"], key="attempt-sibling")
        sibling_delivery = await anext(sibling)
        before = f.command("stats")
        f.command("set", skip_create=skipped, idle_create=int(idle))
        owned = parent.watch(["b", "a", "a"], key="attempt-owned", lease_ms=5000)
        assert (await owned.ready()).kind == "unknown"
        report = await owned.aclose()
        assert report.kind == ("unknown" if skipped == 6 else "closed")
        assert report.local_joined and owned.closed
        stats = f.command("stats")
        all_calls = [x for x in stats["log"] if x["key"] == "attempt-owned"]
        injected = [x for x in all_calls if x.get("fixture")]
        assert [x["op"] for x in injected] == (["watchAck", "watchSub"] if idle and skipped == 5 else [])
        calls = [x for x in all_calls if not x.get("fixture")]
        assert [x["op"] for x in calls] == ["watchSub"] * 6 + ([] if skipped == 6 else ["watchUnsub"])
        assert len({x["request"] for x in calls if x["op"] == "watchSub"}) == 1
        assert len({x["session"] for x in calls}) == 1
        assert stats["committed"] == before["committed"] + (skipped == 5)
        assert stats["active"] == 1
        assert stats["bindings"] == 1 + (skipped == 5)
        assert (await sibling.ack(sibling_delivery)).kind == "acknowledged"
        assert len(await parent.read('id==@parent')) == 1
        await parent.close()
        await f.wait(lambda s: s["active"] == 0 and s["bindings"] == 0)
    with Fixture(watch_binary) as f:
        run(exercise(f))


def test_unconfirmed_reservations_exhaust_bounded_capacity(watch_binary):
    async def exercise(f):
        parent = await client(f)
        f.command("set", skip_create=10000)
        for index in range(32):
            watch = parent.watch(["a"], key=f"uncertain-{index}", lease_ms=1000)
            assert (await watch.ready()).kind == "unknown"
            report = await watch.aclose()
            assert report.kind == "unknown" and report.local_joined
            del watch
        before = f.command("stats")
        assert before["active"] == before["bindings"] == 0
        with pytest.raises(rh.ClientError, match="key"):
            parent.watch(["a"], key="uncertain-0", lease_ms=1000)
        with pytest.raises(rh.ClientError):
            parent.watch(["a"], key="no-reservation-slot")
        assert f.command("stats")["log"] == before["log"]
        assert len(await parent.read('id==@parent')) == 1
        await parent.close()
    with Fixture(watch_binary) as f:
        run(exercise(f))


def test_not_created_and_confirmed_closed_release_key_reservation(watch_binary):
    async def exercise(f):
        parent = await client(f)
        unstarted = parent.watch(["a"], key="reusable")
        assert (await unstarted.aclose()).kind == "not_created"
        reused = parent.watch(["a"], key="reusable")
        assert isinstance(await anext(reused), rh.WatchDelivery)
        assert (await reused.aclose()).kind == "closed"
        # Local ownership can be reused after confirmed close. The server's
        # original same-session tombstone still reports Closed for this key.
        tombstone = parent.watch(["a"], key="reusable")
        assert (await tombstone.ready()).kind == "closed"
        assert (await tombstone.aclose()).kind == "closed"
        fresh = parent.watch(["b"], key="new-observation")
        assert isinstance(await anext(fresh), rh.WatchDelivery)
        await parent.close()
        await f.wait(lambda s: s["active"] == 0 and s["bindings"] == 0)
    with Fixture(watch_binary) as f:
        run(exercise(f))


def test_correlated_create_idle_can_progress_and_confirm_remote_cleanup(watch_binary):
    async def exercise(f):
        parent = await client(f)
        f.command("set", idle_create=1)
        watch = parent.watch(["a"], key="legal-idle")
        assert (await watch.ready()).kind == "ready"
        assert watch.pending is None and watch.state == "open"
        before = f.command("stats")
        assert before["active"] == 1
        assert [x["op"] for x in before["log"] if x.get("fixture")] == ["watchAck", "watchSub"]
        f.command("update", id="a", value=37)
        delivery = await anext(watch)
        assert not delivery.initial and delivery.changed[0]["value"] == rh.Number(37)
        assert (await watch.ack(delivery)).kind == "acknowledged"
        report = await watch.aclose()
        assert report.kind == "closed" and report.local_joined
        assert f.command("stats")["active"] == 0
        assert len(await parent.read('id==@parent')) == 1
        await parent.close()
    with Fixture(watch_binary) as f:
        run(exercise(f))


def test_idle_context_entry_attachment_failure_keeps_cleanup_identity(watch_binary):
    async def exercise(f):
        parent = await client(f)
        f.command("set", idle_create=1, reject_ws=True)
        watch = parent.watch(["b", "a", "a"], key="idle-attach-failure", lease_ms=5000)
        with pytest.raises(rh.ClientError, match="watch creation: unknown"):
            async with watch:
                raise AssertionError("failed attachment must not enter the body")
        report = await watch.aclose()
        assert report.kind == "closed" and report.local_joined and watch.closed
        stats = f.command("stats")
        calls = [x for x in stats["log"] if x["key"] == "idle-attach-failure"]
        assert [x["op"] for x in calls if not x.get("fixture")] == ["watchSub", "watchUnsub"]
        assert [x["op"] for x in calls if x.get("fixture")] == ["watchAck", "watchSub"]
        assert len({x["request"] for x in calls if x["op"] == "watchSub"}) == 1
        assert len({x["session"] for x in calls}) == 1
        assert stats["active"] == 0 and stats["committed"] == 1
        assert len(await parent.read('id==@parent')) == 1
        await parent.close()
    with Fixture(watch_binary) as f:
        run(exercise(f))
