"""Process-isolated checks for the private shared bridge exit hook."""

import subprocess
import sys


def test_terminal_notification_after_owning_loop_close_is_quiet():
    code = r'''
import atexit
import asyncio
import threading

entered = threading.Event()
release = threading.Event()
checked = threading.Event()
retained = []
main_thread = threading.get_ident()

def verify_drained():
    assert checked.is_set(), "shutdown hook returned before its notification drained"
    assert not retained[1].done()

# LIFO exit order: release this admitted callback, drain the native bridge, then
# inspect its completed closed-loop check while the interpreter is still intact.
atexit.register(verify_drained)
import rusty_haystack as rh
atexit.register(release.set)

async def main():
    app = rh.ApplicationBuilder().start()
    retained.append(app)
    await app.ready()
    loop = asyncio.get_running_loop()
    original_is_closed = loop.is_closed
    def held_is_closed():
        if threading.get_ident() != main_thread and not entered.is_set():
            entered.set()
            assert release.wait(3), "closed-loop check was not released"
            assert original_is_closed(), "owning loop was not closed before notification"
            checked.set()
        return original_is_closed()
    loop.is_closed = held_is_closed
    retained.append(app.read("site"))
    deadline = loop.time() + 2
    while not entered.is_set():
        assert loop.time() < deadline, "native notifier did not check its owning loop"
        await asyncio.sleep(.001)
    return loop

loop = asyncio.run(main())
assert loop.is_closed()
assert not checked.is_set()
# Leave the native callback waiting until ordinary atexit. No late
# call_soon_threadsafe traceback should reach stderr during its release/drain.
'''
    result = subprocess.run(
        [sys.executable, "-I", "-X", "faulthandler", "-c", code],
        capture_output=True, text=True, timeout=8,
    )
    assert result.returncode == 0, result.stderr
    assert result.stderr == "", result.stderr


def test_registered_shutdown_hook_rejects_later_future_admission():
    code = r'''
import atexit
import asyncio

registered = []
original_register = atexit.register
def capture(callback, *args, **kwargs):
    registered.append(callback)
    return original_register(callback, *args, **kwargs)
atexit.register = capture
try:
    import rusty_haystack as rh
finally:
    atexit.register = original_register

hooks = [callback for callback in registered if callback.__name__ == "_shutdown_bridge"]
assert len(hooks) == 1, "native module did not register its shutdown hook"
assert not hasattr(rh, "_shutdown_bridge"), "shutdown hook is private"

async def main():
    app = rh.ApplicationBuilder().start()
    await app.ready()
    await app.close()
    hooks[0]()
    hooks[0]()  # Normal atexit will call it once more; shutdown is idempotent.
    try:
        app.ready()
    except RuntimeError as error:
        assert "bridge is shutting down" in str(error)
    else:
        raise AssertionError("bridge admitted a Future after shutdown")

asyncio.run(main())
'''
    result = subprocess.run(
        [sys.executable, "-I", "-X", "faulthandler", "-c", code],
        capture_output=True, text=True, timeout=8,
    )
    assert result.returncode == 0, result.stderr
    assert result.stderr == "", result.stderr
