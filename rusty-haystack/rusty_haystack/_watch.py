"""Private watch entry guard; a native Future completion is not Task handoff."""
from collections.abc import Coroutine


async def _enter(owner, guard):
    try:
        # Create the observation only when polled, avoiding an unobserved native
        # failure Future if this coroutine is closed before it ever starts.
        report = await owner.ready()
        if report.kind != "ready":
            from rusty_haystack import ClientError
            raise ClientError(f"watch creation: {report.kind}/{report.reason}")
    except BaseException:
        # Observe cancellation in the awaiting Task, including when ready has
        # already completed and cannot itself be cancelled anymore.
        guard.close()
        raise
    # No suspension separates this acknowledgement from returning to the
    # caller's async-with protocol.
    guard.entered()
    return owner


class Entry(Coroutine):
    """Also observe throw/close before the wrapped coroutine's first poll.

    CPython can retain an unstarted coroutine's arguments after close or throw.
    Its eventual frame destruction cannot be the only shutdown notification.
    """
    __slots__ = ("_coroutine", "_guard")

    def __init__(self, owner, guard):
        self._guard = guard
        self._coroutine = _enter(owner, guard)

    def __await__(self):
        return self

    def __iter__(self):
        return self

    def __next__(self):
        return self.send(None)

    def send(self, value):
        try:
            return self._coroutine.send(value)
        except BaseException:
            # StopIteration after success is harmless: the guard is disarmed.
            self._guard.close()
            raise

    def throw(self, typ, *args):
        self._guard.close()
        return self._coroutine.throw(typ, *args)

    def close(self):
        self._guard.close()
        return self._coroutine.close()

    def __del__(self):
        self.close()
