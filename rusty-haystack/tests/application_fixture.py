"""Independent standard-library credentials, HTTP and asyncio consumer fixtures."""
import asyncio
import base64
import hashlib
import hmac
import http.client

import rusty_haystack as rh


def auth_manager():
    salt = b"embedded-fixture-salt"
    salted = hashlib.pbkdf2_hmac("sha256", b"password", salt, 4096)
    stored = hashlib.sha256(hmac.digest(salted, b"Client Key", "sha256")).digest()
    server = hmac.digest(salted, b"Server Key", "sha256")
    encoded = ":".join((base64.b64encode(salt).decode(), "4096", base64.b64encode(stored).decode(), base64.b64encode(server).decode()))
    return rh.server.AuthManager.from_toml_str(
        f'[users.reader]\npassword_hash = "{encoded}"\npermissions = ["read"]\n'
        f'[users.denied]\npassword_hash = "{encoded}"\npermissions = []\n'
        f'[users.admin]\npassword_hash = "{encoded}"\npermissions = ["read", "admin"]\n'
    )


def request(address, path, body=None):
    connection = http.client.HTTPConnection(*address, timeout=5)
    try:
        connection.request("POST" if body is not None else "GET", path, body=body,
                           headers={"Content-Type": "text/zinc"})
        response = connection.getresponse()
        return response.status, response.read()
    finally:
        connection.close()


def ids(grid):
    return [row["id"].val for row in grid]


async def cancel_entry_after_completion(app, *, catch_cancellation=False):
    """Cancel the awaiting Task after its ordinary Future completes successfully."""
    loop = asyncio.get_running_loop()
    original_factory = loop.create_future
    had_override = "create_future" in vars(loop)
    created = []
    completed_successfully = []
    body_entered = False
    cancellation_seen = False

    def cancel_waiter(future):
        completed_successfully.append(
            future.done() and not future.cancelled() and future.exception() is None
        )
        task.cancel()

    def create_future():
        future = original_factory()
        created.append(future)
        # This callback precedes Task's wakeup callback on the same real Future.
        future.add_done_callback(cancel_waiter)
        return future

    async def enter():
        nonlocal body_entered, cancellation_seen
        try:
            async with app:
                body_entered = True
        except asyncio.CancelledError:
            cancellation_seen = True
            if not catch_cancellation:
                raise

    loop.create_future = create_future
    try:
        task = asyncio.create_task(enter())
        # A task turn installs the entry Future and suspends the waiter. This
        # does not wait for readiness or use elapsed time as the race oracle.
        await asyncio.sleep(0)
    finally:
        if had_override:
            loop.create_future = original_factory
        else:
            del loop.create_future
    try:
        await task
    except asyncio.CancelledError:
        assert not catch_cancellation
    assert len(created) == 1
    assert completed_successfully == [True]
    assert cancellation_seen and not body_entered
    assert task.cancelled() is not catch_cancellation
    assert app.state in ("closing", "closed"), app.state
    await asyncio.wait_for(app.terminated(), 2)
    assert app.closed
