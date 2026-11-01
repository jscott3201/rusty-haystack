"""Process-owned real Rust server, with bounded private control-pipe requests."""
import asyncio
import json
import os
from pathlib import Path
import select
import subprocess
import time

ROOT = Path(__file__).resolve().parents[2]

def build_fixture():
    if os.environ.get("HAYSTACK_WATCH_FIXTURE"):
        return Path(os.environ["HAYSTACK_WATCH_FIXTURE"]).resolve()
    subprocess.run(["cargo", "+1.99.1", "build", "--locked", "-p", "rusty-haystack-server", "--example", "python_watch_fixture"], cwd=ROOT, check=True, timeout=300)
    return ROOT / "target/debug/examples/python_watch_fixture"

class Fixture:
    def __init__(self, binary, *, direct=False):
        self.process = subprocess.Popen([str(binary)] + (["--direct"] if direct else []), stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, bufsize=1)
        self.stopped = False
        try:
            self.url = self.receive()["url"]
        except BaseException:
            self.stopped = True
            self.dispose()
            raise

    def receive(self):
        if not select.select([self.process.stdout], [], [], 10)[0]:
            self.process.kill()
            raise AssertionError("first-party fixture control response timed out")
        line = self.process.stdout.readline()
        if not line:
            raise AssertionError(f"first-party fixture exited: {self.process.stderr.read()}")
        return json.loads(line)

    def command(self, op, **values):
        self.process.stdin.write(json.dumps({"op": op, **values}) + "\n")
        self.process.stdin.flush()
        return self.receive()

    async def wait(self, predicate, timeout=5):
        deadline = time.monotonic() + timeout
        while True:
            stats = self.command("stats")
            if predicate(stats):
                return stats
            if time.monotonic() >= deadline:
                raise AssertionError(f"fixture condition timed out: {stats}")
            await asyncio.sleep(.005)

    def close(self):
        if self.stopped:
            return
        self.stopped = True
        try:
            report = self.command("stop")
            assert report == {"stopped": True, "active": 0, "bindings": 0, "listener_reused": True}, report
            self.process.wait(timeout=10)
            assert self.process.returncode == 0, self.process.stderr.read()
        finally:
            self.dispose()

    def dispose(self):
        if self.process.poll() is None:
            self.process.kill()
        self.process.wait(timeout=5)
        for stream in (self.process.stdin, self.process.stdout, self.process.stderr):
            stream.close()

    def __enter__(self):
        return self
    def __exit__(self, *_):
        self.close()

async def cancel_entry_after_completion(watch):
    """Cancel Task handoff after successful native Future completion."""
    loop = asyncio.get_running_loop()
    original_factory = loop.create_future
    had_override = "create_future" in vars(loop)
    created, completed = [], []
    entered = False
    def cancel_waiter(future):
        completed.append(future.done() and not future.cancelled() and future.exception() is None)
        task.cancel()
    def create_future():
        future = original_factory()
        created.append(future)
        future.add_done_callback(cancel_waiter)
        return future
    async def enter():
        nonlocal entered
        async with watch:
            entered = True
    loop.create_future = create_future
    try:
        task = asyncio.create_task(enter())
        await asyncio.sleep(0)
    finally:
        if had_override:
            loop.create_future = original_factory
        else:
            del loop.create_future
    try:
        await task
    except asyncio.CancelledError:
        pass
    else:
        raise AssertionError("entry cancellation did not reach awaiting Task")
    assert len(created) == 1 and completed == [True] and not entered
    assert watch.state in ("closing", "closed")
    report = await watch.aclose()
    assert report.kind == "closed" and report.local_joined and watch.closed
