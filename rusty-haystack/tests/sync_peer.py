"""Run the independent Python HTTP peer beside a plain synchronous caller."""
import asyncio
import queue
import threading

from pathlib import Path
import runpy

Peer = runpy.run_path(str(Path(__file__).with_name("async_peer.py")))["Peer"]


class ThreadedPeer:
    def __init__(self, **kwargs):
        self.kwargs = kwargs
        self.ready = queue.Queue()
        self.errors = []

    def __enter__(self):
        def worker():
            async def serve():
                async with Peer(**self.kwargs) as peer:
                    self.peer = peer
                    self.loop = asyncio.get_running_loop()
                    self.stop = asyncio.Event()
                    self.ready.put(peer.url)
                    await self.stop.wait()
            try:
                asyncio.run(serve())
            except BaseException as error:
                self.errors.append(error)
                self.ready.put(error)
        self.thread = threading.Thread(target=worker)
        self.thread.start()
        result = self.ready.get(timeout=5)
        if isinstance(result, BaseException):
            self.thread.join(5)
            raise result
        self.url = result
        return self

    def call(self, callback):
        async def invoke():
            return callback(self.peer)
        return asyncio.run_coroutine_threadsafe(invoke(), self.loop).result(5)

    @property
    def requests(self):
        return self.call(lambda peer: list(peer.requests))

    def set_mode(self, mode):
        self.call(lambda peer: setattr(peer, "mode", mode))

    def wait_eof(self, count=1):
        asyncio.run_coroutine_threadsafe(self.peer.wait_eof(count), self.loop).result(6)

    def wait_idle(self):
        asyncio.run_coroutine_threadsafe(self.peer.wait_idle(), self.loop).result(6)

    def wait_requests(self, count):
        asyncio.run_coroutine_threadsafe(self.peer.wait_requests(count), self.loop).result(6)

    def __exit__(self, *args):
        self.loop.call_soon_threadsafe(self.stop.set)
        self.thread.join(7)
        assert not self.thread.is_alive(), "independent HTTP peer did not stop"
        if self.errors:
            raise self.errors[0]
