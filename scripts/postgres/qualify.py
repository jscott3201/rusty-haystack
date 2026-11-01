#!/usr/bin/env python3
"""Run the explicit PostgreSQL 18.6 qualification profile; never silently skip.

Use --docker for two disposable loopback fixtures, or supply both private fixture
JSON paths. Docker resources are unique to this invocation and removed in finally.
No connection URL, password, certificate key, environment dump or raw Docker
failure output is printed. Cargo test output contains bounded application errors.
"""
import argparse
import json
import os
from pathlib import Path
import secrets
import shutil
import subprocess
import sys
import tempfile
import time
import uuid

ROOT = Path(__file__).resolve().parents[2]
IMAGE = "postgres:18.6@sha256:74935e72241653ca55e0414067e6d8763aceb8a810eb51b452253ec3dcfc4336"


class FixtureError(RuntimeError):
    pass


def native(*args, timeout=120):
    """Bounded native operation; never replay potentially sensitive diagnostics."""
    try:
        result = subprocess.run(args, check=False, capture_output=True, text=True, timeout=timeout)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise FixtureError(f"{args[0]} fixture operation failed") from error
    if result.returncode:
        raise FixtureError(f"{args[0]} fixture operation failed (exit {result.returncode})")
    return result.stdout.strip()


def private_write(path, text):
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "w") as output:
        output.write(text)


class Fixtures:
    def __init__(self):
        self.directory = Path(tempfile.mkdtemp(prefix="rusty-haystack-postgres-"))
        self.token = uuid.uuid4().hex[:16]
        self.containers = []
        self.volumes = []

    def command(self, container, sql):
        return native("docker", "exec", container, "psql", "-U", "haystack_test", "-d", "haystack_test",
                      "-v", "ON_ERROR_STOP=1", "-At", "-c", sql)

    def start(self, plain=False):
        label = "plain" if plain else "tls"
        container = f"rusty-haystack-pg-{self.token}-{label}"
        password = secrets.token_urlsafe(32)
        environment = self.directory / f"{label}.env"
        private_write(environment, f"POSTGRES_USER=haystack_test\nPOSTGRES_DB=haystack_test\nPOSTGRES_PASSWORD={password}\n")
        args = ["docker", "run", "--detach", "--name", container,
                "--label", f"rusty-haystack.qualification={self.token}",
                "--memory", "512m" if plain else "1g", "--cpus", "1" if plain else "2",
                "--publish", "127.0.0.1::5432", "--env-file", str(environment)]
        if plain:
            args += ["--tmpfs", "/var/lib/postgresql"]
        else:
            volume = f"rusty-haystack-pg-{self.token}"
            native("docker", "volume", "create", "--label", f"rusty-haystack.qualification={self.token}", volume)
            self.volumes.append(volume)
            # PostgreSQL 18 uses this parent mount; the old /data mount is wrong.
            args += ["--volume", f"{volume}:/var/lib/postgresql"]
        self.containers.append(container)
        native(*args, IMAGE, "postgres", "-c", "max_connections=40", "-c", "fsync=on",
               "-c", "synchronous_commit=on", "-c", "full_page_writes=on", timeout=300)
        expires = time.monotonic() + 60
        while True:
            try:
                # The official image's temporary initialization server accepts
                # Unix sockets only. Require the final server's TCP listener.
                native("docker", "exec", container, "pg_isready", "-h", "127.0.0.1",
                       "-U", "haystack_test", "-d", "haystack_test", timeout=5)
                break
            except FixtureError:
                if time.monotonic() >= expires:
                    raise FixtureError("PostgreSQL fixture readiness timed out") from None
                time.sleep(0.2)
        public = native("docker", "port", container, "5432/tcp")
        if not public.startswith("127.0.0.1:") or "\n" in public:
            raise FixtureError("fixture must bind exactly one loopback port")
        port = int(public.rsplit(":", 1)[1])
        settings = self.command(container, "SELECT current_setting('server_version_num'),current_setting('fsync'),current_setting('synchronous_commit'),current_setting('full_page_writes')")
        if settings != "180006|on|on|on":
            raise FixtureError("fixture version or durability settings mismatch")
        if not plain:
            for name in ("server.key", "server.crt"):
                native("docker", "cp", str(self.directory / name), f"{container}:/var/lib/postgresql/{name}")
                native("docker", "exec", "--user", "0", container, "chown", "postgres:postgres", f"/var/lib/postgresql/{name}")
            native("docker", "exec", "--user", "0", container, "chmod", "600", "/var/lib/postgresql/server.key")
            for setting in ("ssl_cert_file='/var/lib/postgresql/server.crt'", "ssl_key_file='/var/lib/postgresql/server.key'", "ssl='on'"):
                self.command(container, f"ALTER SYSTEM SET {setting}")
            self.command(container, "SELECT pg_reload_conf()")
            expires = time.monotonic() + 10
            while self.command(container, "SHOW ssl") != "on":
                if time.monotonic() >= expires:
                    raise FixtureError("verified TLS fixture did not become ready")
                time.sleep(0.1)
        elif self.command(container, "SHOW ssl") != "off":
            raise FixtureError("negative TLS fixture is not plaintext-only")
        fixture = self.directory / f"{label}.json"
        private_write(fixture, json.dumps({"host": "127.0.0.1", "port": port, "database": "haystack_test",
                      "user": "haystack_test", "password": password, "tls_server_name": "localhost",
                      "root_certificate": str(self.directory / "ca.crt")}))
        print(f"PostgreSQL 18.6 {label} fixture ready on loopback", flush=True)
        return fixture

    def certificates(self):
        path = self.directory
        native("openssl", "req", "-x509", "-newkey", "rsa:2048", "-sha256", "-nodes", "-days", "2",
               "-subj", "/CN=Rusty Haystack disposable qualification CA", "-keyout", str(path / "ca.key"), "-out", str(path / "ca.crt"))
        native("openssl", "req", "-new", "-newkey", "rsa:2048", "-nodes", "-subj", "/CN=localhost",
               "-keyout", str(path / "server.key"), "-out", str(path / "server.csr"))
        private_write(path / "server.ext", "subjectAltName=DNS:localhost,IP:127.0.0.1\nbasicConstraints=CA:FALSE\nkeyUsage=digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n")
        native("openssl", "x509", "-req", "-sha256", "-days", "2", "-in", str(path / "server.csr"),
               "-CA", str(path / "ca.crt"), "-CAkey", str(path / "ca.key"), "-CAcreateserial",
               "-extfile", str(path / "server.ext"), "-out", str(path / "server.crt"))
        for key in (path / "ca.key", path / "server.key"):
            key.chmod(0o600)

    def close(self):
        failed = False
        for container in reversed(self.containers):
            try:
                native("docker", "rm", "--force", container)
            except FixtureError:
                failed = True
        for volume in reversed(self.volumes):
            try:
                native("docker", "volume", "rm", volume)
            except FixtureError:
                failed = True
        shutil.rmtree(self.directory)
        if failed:
            raise FixtureError("task-owned Docker fixture cleanup incomplete")
        print("Task-owned PostgreSQL fixtures removed", flush=True)


def qualify(fixture, plain, toolchain):
    for path in (fixture, plain):
        if path is None or not path.is_file():
            raise FixtureError("both private PostgreSQL fixture files are required")
    environment = os.environ.copy()
    environment["RUSTY_HAYSTACK_POSTGRES_FIXTURE"] = str(fixture.resolve())
    environment["RUSTY_HAYSTACK_POSTGRES_PLAIN_FIXTURE"] = str(plain.resolve())
    command = ["cargo", f"+{toolchain}", "nextest", "run", "--locked", "-p", "rusty-haystack-app",
               "--features", "postgres", "--lib", "-E", "test(postgres::qualification::)",
               "--run-ignored", "ignored-only", "--no-tests", "fail", "--test-threads", "4"]
    return subprocess.run(command, cwd=ROOT, env=environment, check=False).returncode


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--docker", action="store_true", help="create and clean disposable bounded Docker fixtures")
    parser.add_argument("--fixture", type=Path)
    parser.add_argument("--plain-fixture", type=Path)
    parser.add_argument("--toolchain", default="1.99.1")
    args = parser.parse_args()
    if args.docker and (args.fixture or args.plain_fixture):
        parser.error("--docker cannot consume externally owned fixtures")
    fixtures = None
    result = 1
    try:
        if args.docker:
            fixtures = Fixtures()
            fixtures.certificates()
            args.fixture = fixtures.start()
            args.plain_fixture = fixtures.start(plain=True)
        result = qualify(args.fixture, args.plain_fixture, args.toolchain)
    except (FixtureError, OSError) as error:
        print(f"PostgreSQL qualification failed: {error}", file=sys.stderr)
    finally:
        if fixtures:
            try:
                fixtures.close()
            except FixtureError as error:
                print(str(error), file=sys.stderr)
                if result == 0:
                    result = 1
    return result


if __name__ == "__main__":
    sys.exit(main())
