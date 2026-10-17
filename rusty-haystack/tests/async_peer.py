"""Independent loopback HTTP/SCRAM peer with persistent-socket cleanup oracles.

Uses only the Python standard library. Tests must observe EOF before stopping
this fixture: its own teardown is deliberately not a client cleanup receipt.
"""
import asyncio
import base64
import hashlib
import hmac


def b64(data):
    return base64.urlsafe_b64encode(data).decode().rstrip("=")


def unb64(data):
    return base64.urlsafe_b64decode(data + "=" * (-len(data) % 4))


class Peer:
    def __init__(self, *, mode="normal", ssl=None, iterations=1000):
        self.mode = mode
        self.ssl = ssl
        self.iterations = iterations
        self.requests = []
        self.handlers = set()
        self.writers = set()
        self.eof = 0
        self.changed = asyncio.Condition()
        self.exchanges = {}

    async def __aenter__(self):
        self.server = await asyncio.start_server(self.handle, "127.0.0.1", 0, ssl=self.ssl)
        port = self.server.sockets[0].getsockname()[1]
        self.url = f"{'https' if self.ssl else 'http'}://localhost:{port}/api"
        return self

    async def __aexit__(self, *args):
        self.server.close()
        await self.server.wait_closed()
        for writer in list(self.writers):
            writer.close()
        if self.handlers:
            await asyncio.wait_for(asyncio.gather(*self.handlers, return_exceptions=True), 5)

    async def notify(self):
        async with self.changed:
            self.changed.notify_all()

    async def until(self, predicate):
        async with asyncio.timeout(5):
            async with self.changed:
                await self.changed.wait_for(predicate)

    async def wait_requests(self, count):
        await self.until(lambda: len(self.requests) >= count)

    async def wait_idle(self):
        await self.until(lambda: not self.handlers)

    async def wait_eof(self, count=1):
        await self.until(lambda: self.eof >= count)

    async def response(self, writer, status, body=b"", headers=""):
        writer.write(f"HTTP/1.1 {status} Fixture\r\nContent-Type: text/zinc\r\nContent-Length: {len(body)}\r\n{headers}\r\n".encode() + body)
        await writer.drain()

    async def operation(self, reader, writer, method, path, headers, body):
        """Optional authenticated operation for specialized independent peers."""
        return False

    async def handle(self, reader, writer):
        task = asyncio.current_task()
        self.handlers.add(task)
        self.writers.add(writer)
        try:
            while True:
                head = await reader.readuntil(b"\r\n\r\n")
                lines = head.decode("ascii").split("\r\n")
                method, path, _ = lines[0].split()
                headers = {key.lower(): value.strip() for line in lines[1:] if line for key, value in [line.split(":", 1)]}
                body = await reader.readexactly(int(headers.get("content-length", "0")))
                self.requests.append((method, path, headers, body))
                await self.notify()
                auth = headers.get("authorization", "")
                if auth.startswith("HELLO"):
                    if self.mode == "stall_connect":
                        await reader.read()  # Only the client may end this wait.
                        self.eof += 1
                        return
                    await self.response(writer, 401, headers="WWW-Authenticate: SCRAM hash=SHA-256, handshakeToken=fixture\r\n")
                elif auth.startswith("SCRAM"):
                    fields = dict(part.strip().split("=", 1) for part in auth.removeprefix("SCRAM ").split(","))
                    message = unb64(fields["data"]).decode()
                    if message.startswith("n,,"):
                        bare = message[3:]
                        nonce = dict(part.split("=", 1) for part in bare.split(","))["r"] + "server"
                        first = f"r={nonce},s={base64.b64encode(b'fixture-salt').decode()},i={self.iterations}"
                        self.exchanges[nonce] = (bare, first)
                        await self.response(writer, 401, headers=f"WWW-Authenticate: SCRAM hash=SHA-256, handshakeToken=next, data={b64(first.encode())}\r\n")
                    else:
                        final_fields = dict(part.split("=", 1) for part in message.split(","))
                        bare, first = self.exchanges[final_fields["r"]]
                        transcript = f"{bare},{first},{message.rsplit(',p=', 1)[0]}".encode()
                        salted = hashlib.pbkdf2_hmac("sha256", b"password", b"fixture-salt", self.iterations)
                        client_key = hmac.digest(salted, b"Client Key", "sha256")
                        signature = hmac.digest(hashlib.sha256(client_key).digest(), transcript, "sha256")
                        proof = bytes(a ^ b for a, b in zip(client_key, signature))
                        if not hmac.compare_digest(base64.b64decode(final_fields["p"]), proof):
                            await self.response(writer, 401)
                            continue
                        server_key = hmac.digest(salted, b"Server Key", "sha256")
                        verifier = "v=" + base64.b64encode(hmac.digest(server_key, transcript, "sha256")).decode()
                        await self.response(writer, 200, headers=f"Authentication-Info: hash=SHA-256, authToken=fixture-token, data={b64(verifier.encode())}\r\n")
                elif auth == "BEARER authToken=fixture-token" and await self.operation(reader, writer, method, path, headers, body):
                    pass
                elif auth == "BEARER authToken=fixture-token" and path == "/api/read":
                    if self.mode == "stall_read":
                        writer.write(b"HTTP/1.1 200 Fixture\r\nContent-Type: text/zinc\r\nContent-Length: 100\r\n\r\nv")
                        await writer.drain()
                        await reader.read()
                        self.eof += 1
                        return
                    if self.mode == "oversize_declared":
                        writer.write(b"HTTP/1.1 200 Fixture\r\nContent-Length: 1048577\r\n\r\n")
                        await writer.drain()
                        await reader.read()
                        self.eof += 1
                        return
                    if self.mode in ("oversize_chunked", "oversize_unknown"):
                        headers = b"Transfer-Encoding: chunked\r\n" if self.mode == "oversize_chunked" else b"Connection: close\r\n"
                        data = b"x" * 1048577
                        writer.write(b"HTTP/1.1 200 Fixture\r\n" + headers + b"\r\n")
                        writer.write(f"{len(data):x}\r\n".encode() + data + b"\r\n" if self.mode == "oversize_chunked" else data)
                        await writer.drain()
                        await reader.read()
                        self.eof += 1
                        return
                    if self.mode == "error_status":
                        await self.response(writer, 500, b"private-peer-sentinel")
                    elif self.mode == "error_grid":
                        await self.response(writer, 200, b'ver:"3.0" err dis:"private-peer-sentinel"\nempty\n')
                    elif self.mode == "invalid_grid":
                        await self.response(writer, 200, b"private-peer-sentinel")
                    else:
                        rows = 2 if self.mode == "excess_rows" else 1
                        grid = 'ver:"3.0"\nid,dis\n' + ''.join(f'@r{i},"Site"\n' for i in range(rows))
                        await self.response(writer, 200, grid.encode())
                else:
                    await self.response(writer, 401)
        except (asyncio.IncompleteReadError, ConnectionResetError, BrokenPipeError):
            self.eof += 1
        finally:
            writer.close()
            try:
                await writer.wait_closed()
            except (ConnectionError, OSError):
                pass
            self.writers.discard(writer)
            self.handlers.discard(task)
            await self.notify()


def tls_material(directory):
    """Fresh, seven-day synthetic certificates; independent OpenSSL test CA.

    Short validity is required by the macOS platform verifier. No fixture key or
    certificate has external authority or survives the test's temporary folder.
    """
    import shutil
    import subprocess
    openssl = shutil.which("openssl")
    if not openssl:
        raise RuntimeError("async TLS tests require the OpenSSL CLI")
    def invoke(*args):
        subprocess.run([openssl, *args], check=True, capture_output=True, timeout=15)
    invoke("req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", str(directory / "ca-key.pem"), "-out", str(directory / "ca.pem"), "-days", "7", "-subj", "/CN=HaystackAsyncTestCA", "-addext", "basicConstraints=critical,CA:TRUE")
    invoke("req", "-new", "-newkey", "rsa:2048", "-nodes", "-keyout", str(directory / "localhost-key.pem"), "-out", str(directory / "localhost.csr"), "-subj", "/CN=localhost")
    extension = directory / "localhost.ext"
    extension.write_text("subjectAltName=DNS:localhost\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth,clientAuth\n")
    invoke("x509", "-req", "-in", str(directory / "localhost.csr"), "-CA", str(directory / "ca.pem"), "-CAkey", str(directory / "ca-key.pem"), "-set_serial", "2", "-out", str(directory / "localhost.pem"), "-days", "7", "-extfile", str(extension))
    return directory
