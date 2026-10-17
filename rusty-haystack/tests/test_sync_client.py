"""Every blocking/native case has an external process deadline."""
from pathlib import Path
import subprocess
import sys
import textwrap

import pytest


def script(source, timeout=20):
    fixture = str(Path(__file__).parent.resolve())
    prelude = f"import sys; sys.path.insert(0, {fixture!r})\n"
    prelude += "import asyncio, gc, threading, time\nimport rusty_haystack as rh\nfrom sync_peer import ThreadedPeer\n"
    result = subprocess.run([sys.executable, "-c", prelude + textwrap.dedent(source)],
                            capture_output=True, text=True, timeout=timeout)
    assert result.returncode == 0, result.stdout + result.stderr
    assert not any(word in result.stderr.lower() for word in ("panicked", "fatal python", "unawaited", "never awaited")), result.stderr


def test_plain_script_context_returns_values_and_closes_native_io():
    script('''
        from rusty_haystack.client import SyncHaystackClient, HaystackClient, WsClient
        import inspect
        assert SyncHaystackClient is rh.SyncHaystackClient
        assert inspect.signature(SyncHaystackClient).parameters["timeout"].default == 30.0
        assert inspect.signature(SyncHaystackClient.read).parameters["limit"].default == 100
        assert all(hasattr(HaystackClient, name) for name in ("about", "point_write", "his_read", "his_write", "invoke_action", "call", "watch_sub"))
        assert hasattr(WsClient, "read")
        with ThreadedPeer() as peer:
            client = rh.SyncHaystackClient(peer.url, "user", "password")
            assert client.state == "new" and not client.closed
            with client as same:
                assert same is client and client.state == "open"
                try: client.connect()
                except rh.ClientError as error: assert "new client" in str(error)
                else: raise AssertionError("duplicate connect accepted")
                grid = client.read("site", limit=1)
                assert isinstance(grid, rh.HGrid) and len(grid) == 1
                assert grid[0]["id"].val == "r0"
                request = rh.decode_grid("text/zinc", peer.requests[-1][3].decode())
                assert request[0]["filter"] == "site" and request[0]["limit"].val == 1
            assert client.closed and client.state == "closed"
            peer.wait_eof(); peer.wait_idle()
            assert client.close() is None
            assert len(peer.requests) == 4
    ''')


def test_loop_rejection_and_invalid_first_use_do_not_bind_or_dispatch():
    script('''
        with ThreadedPeer() as peer:
            client = rh.SyncHaystackClient(peer.url, "user", "password")
            async def misuse():
                for op in (client.connect, client.close, lambda: client.read("site")):
                    try: op()
                    except RuntimeError as error: assert "asyncio" in str(error)
                    else: raise AssertionError("active loop accepted")
            asyncio.run(misuse())
            for op in (lambda: client.connect(timeout=float("nan")), lambda: client.close(timeout=0), lambda: client.read("x" * 8193), lambda: client.read("site", limit=0)):
                try: op()
                except ValueError: pass
                else: raise AssertionError("invalid input accepted")
            assert client.state == "new" and peer.requests == []
            try: client.read("site")
            except rh.ClientError: pass
            else: raise AssertionError("unconnected read accepted")
            errors = []
            def whole_flow():
                try:
                    with client: assert len(client.read("site")) == 1
                except BaseException as error: errors.append(error)
            thread = threading.Thread(target=whole_flow); thread.start(); thread.join(8)
            assert not thread.is_alive() and not errors, errors
            assert client.closed
            for op in (client.connect, client.close, lambda: client.read("site")):
                try: op()
                except RuntimeError as error: assert "OS thread" in str(error)
                else: raise AssertionError("foreign thread accepted")
            assert len(peer.requests) == 4
            request = rh.decode_grid("text/zinc", peer.requests[-1][3].decode())
            assert request[0]["limit"].val == 100
            peer.wait_eof(); peer.wait_idle()
    ''')


def test_rejected_open_loop_thread_and_invalid_input_preserve_owner():
    script('''
        with ThreadedPeer() as peer:
            client = rh.SyncHaystackClient(peer.url, "user", "password")
            assert client.connect() is None
            async def misuse():
                for op in (client.close, lambda: client.read("site")):
                    try: op()
                    except RuntimeError: pass
                    else: raise AssertionError("active loop accepted")
            asyncio.run(misuse())
            for timeout in [0, -1, float("nan"), float("inf"), 1e100, 1e-100]:
                for op in (lambda: client.read("site", timeout=timeout), lambda: client.close(timeout=timeout)):
                    try: op()
                    except ValueError: pass
                    else: raise AssertionError("invalid timeout accepted")
            for limit in [0, -1, 1001, 2**100]:
                try: client.read("site", limit=limit)
                except (ValueError, OverflowError): pass
                else: raise AssertionError("invalid limit accepted")
            for value in ["x" * 8193, "é" * 4097]:
                try: client.read(value)
                except ValueError: pass
                else: raise AssertionError("oversized filter accepted")
            assert client.state == "open" and len(peer.requests) == 3
            errors = []
            def foreign():
                for op in (client.close, lambda: client.read("site")):
                    try: op()
                    except RuntimeError as error: errors.append(str(error))
            thread = threading.Thread(target=foreign); thread.start(); thread.join(5)
            assert len(errors) == 2 and len(peer.requests) == 3
            assert len(client.read("site")) == 1
            client.close(); peer.wait_eof(); peer.wait_idle()
    ''')


@pytest.mark.parametrize("timeout", ["0", "-1", "float('nan')", "float('inf')", "1e100", "1e-100"])
def test_invalid_constructor_timeout(timeout):
    script(f'''
        try: rh.SyncHaystackClient("http://localhost:1", "user", "password", timeout={timeout})
        except ValueError: pass
        else: raise AssertionError("invalid constructor timeout accepted")
    ''')


@pytest.mark.parametrize("default,override", [(0.1, 10), (5, 0.1)])
def test_read_budget_only_shortens_and_timeout_recovery(default, override):
    script(f'''
        with ThreadedPeer(mode="stall_read") as peer:
            client = rh.SyncHaystackClient(peer.url, "user", "password", timeout={default})
            client.connect()
            started = time.monotonic()
            try: client.read("site", timeout={override})
            except TimeoutError: pass
            else: raise AssertionError("stalled read succeeded")
            assert .05 <= time.monotonic() - started < 2
            assert client.state == "open"
            peer.wait_eof()
            peer.set_mode("normal")
            assert len(client.read("site")) == 1
            client.close(); peer.wait_idle()
    ''')


def test_connect_timeout_is_terminal_and_drop_cleans_up():
    script('''
        with ThreadedPeer(mode="stall_connect") as peer:
            client = rh.SyncHaystackClient(peer.url, "user", "password")
            try: client.connect(timeout=.05)
            except TimeoutError: pass
            else: raise AssertionError("stalled connect succeeded")
            try: client.connect()
            except rh.ClientError: pass
            else: raise AssertionError("failed connect reused")
            client.close(); assert client.closed
            peer.wait_eof(); peer.wait_idle()
        with ThreadedPeer() as peer:
            client = rh.SyncHaystackClient(peer.url, "user", "password")
            client.connect(); del client; gc.collect()
            peer.wait_eof(); peer.wait_idle()
    ''')


def test_tiny_close_budget_still_starts_cleanup():
    script('''
        with ThreadedPeer() as peer:
            client = rh.SyncHaystackClient(peer.url, "user", "password")
            client.connect()
            try: client.close(timeout=1e-9)
            except TimeoutError: pass
            else: raise AssertionError("expired close published success")
            assert client.state in ("closing", "closed")
            client.close(); assert client.closed
            peer.wait_eof(); peer.wait_idle()
    ''')


@pytest.mark.parametrize("mode", ["oversize_declared", "oversize_chunked", "oversize_unknown", "excess_rows", "error_status", "error_grid", "invalid_grid"])
def test_bounded_responses(mode):
    script(f'''
        with ThreadedPeer(mode={mode!r}) as peer:
            with rh.SyncHaystackClient(peer.url, "user", "password") as client:
                try: client.read("site", limit=1)
                except rh.ClientError as error: assert "private-peer-sentinel" not in str(error)
                else: raise AssertionError("bad response accepted")
            peer.wait_idle()
    ''')


def test_context_preserves_body_exception():
    script('''
        with ThreadedPeer() as peer:
            client = rh.SyncHaystackClient(peer.url, "user", "password")
            body = ValueError("body")
            try:
                with client: raise body
            except ValueError as error: assert error is body
            else: raise AssertionError("body swallowed")
            assert client.closed; peer.wait_eof(); peer.wait_idle()
    ''')


def test_context_exit_accepts_public_keyword_names():
    script('''
        with ThreadedPeer() as peer:
            client = rh.SyncHaystackClient(peer.url, "user", "password")
            client.connect()
            try:
                assert client.__exit__(exc_type=None, exc_value=None, traceback=None) is None
                assert client.closed
                peer.wait_eof(); peer.wait_idle()
            finally:
                client.close()
    ''')


@pytest.mark.parametrize("trust", ["verified", "default_reject", "explicit_lab"])
def test_sync_tls_policy(trust):
    script(f'''
        from pathlib import Path
        import ssl, tempfile
        from async_peer import tls_material
        from rusty_haystack.client import TlsConfig
        with tempfile.TemporaryDirectory() as temporary:
            directory = tls_material(Path(temporary))
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            context.load_cert_chain(directory / "localhost.pem", directory / "localhost-key.pem")
            tls = TlsConfig.with_ca((directory / "ca.pem").read_bytes()) if {trust!r} == "verified" else None
            with ThreadedPeer(ssl=context) as peer:
                client = rh.SyncHaystackClient(peer.url, "user", "password", tls=tls, tls_verify={trust!r} != "explicit_lab")
                if {trust!r} == "default_reject":
                    try: client.connect()
                    except rh.ClientError: pass
                    else: raise AssertionError("untrusted TLS accepted")
                    client.close()
                    assert peer.requests == []
                else:
                    with client: assert len(client.read("site")) == 1
                    peer.wait_eof(); peer.wait_idle()
                assert client.closed
    ''')


def test_normal_process_exit_with_native_wait_pending():
    # The outer process owns the peer and observes EOF before fixture teardown.
    # Pending daemon work is an exit qualification case, not a close receipt.
    child_source = """
import sys, threading
import rusty_haystack as rh
def pending():
    client = rh.SyncHaystackClient(sys.argv[1], "user", "password")
    client.connect()
    client.read("site")
threading.Thread(target=pending, daemon=True).start()
sys.stdin.readline()
"""
    script(f'''
        import subprocess
        child_source = {child_source!r}
        with ThreadedPeer(mode="stall_read") as peer:
            child = subprocess.Popen([sys.executable, "-c", child_source, peer.url], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            try:
                peer.wait_requests(4)
                stdout, stderr = child.communicate("exit now\\n", timeout=6)
                assert child.returncode == 0, stdout + stderr
                assert not stderr, stderr
                peer.wait_eof(); peer.wait_idle()
            finally:
                if child.poll() is None:
                    child.kill(); child.wait()
    ''')
