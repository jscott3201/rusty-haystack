#!/usr/bin/env python3
"""Install and exercise the core-plus-PostgreSQL standalone CLI on a disposable DB.

Requires a POSIX host with ps, Docker, OpenSSL, cargo-nextest and the pinned Rust
toolchain. This selected profile owns
all processes, credentials, configuration, database schema and installation it
creates. It never accepts an existing database or silently skips runtime work.
"""
import argparse
import json
import os
from pathlib import Path
import queue
import secrets
import signal
import socket
import subprocess
import sys
import threading
import time

from qualify import Fixtures, FixtureError, ROOT, private_write


# Nextest gives each test its own process group, but keeps the inherited session.
# A new session therefore bounds our ownership without relying on a single group
# or inspecting commands, arguments, credentials or environment in the process list.
CANCEL_GRACE = 10
TERMINATE_GRACE = 5
KILL_GRACE = 5


def session_groups(session):
    try:
        listing = subprocess.run(["ps", "-axo", "pid=,stat="], check=True,
                                 capture_output=True, text=True, timeout=5)
        groups = set()
        for line in listing.stdout.splitlines():
            pid_text, state = line.split(maxsplit=1)
            # Zombies have stopped; only their parent can reap them. checked()
            # always reaps its direct child before reporting cleanup complete.
            if state.startswith("Z"):
                continue
            pid = int(pid_text)
            try:
                if os.getsid(pid) == session:
                    groups.add(os.getpgid(pid))
            except ProcessLookupError:
                pass
        return groups
    except (OSError, subprocess.SubprocessError, ValueError):
        raise FixtureError("cannot establish standalone command session ownership") from None


def signal_groups(groups, selected_signal):
    for group in groups:
        try:
            os.killpg(group, selected_signal)
        except ProcessLookupError:
            pass


def wait_session_empty(process, timeout):
    deadline = time.monotonic() + timeout
    while True:
        status = process.poll()  # Reap our child even if a test outlives its runner.
        if not session_groups(process.pid) and status is not None:
            return True
        if time.monotonic() >= deadline:
            return False
        time.sleep(0.05)


def stop_command(process):
    groups = session_groups(process.pid)
    # Prefer the runner's own graceful cancellation/forwarding and child reaping.
    # If it already exited, interrupt the remaining owned groups directly.
    signal_groups({process.pid} if process.pid in groups else groups, signal.SIGINT)
    if wait_session_empty(process, CANCEL_GRACE):
        return
    for selected_signal, grace in ((signal.SIGTERM, TERMINATE_GRACE),
                                   (signal.SIGKILL, KILL_GRACE)):
        signal_groups(session_groups(process.pid), selected_signal)
        if wait_session_empty(process, grace):
            return
    raise FixtureError("standalone command session cleanup incomplete")


def checked(command, environment=None, timeout=900):
    if os.name != "posix":
        raise FixtureError("standalone command cleanup requires POSIX process sessions")
    # Verify the observer exists before starting any child that may need it.
    session_groups(-1)
    try:
        process = subprocess.Popen(command, cwd=ROOT, env=environment, start_new_session=True)
    except OSError:
        raise FixtureError("standalone build or consumer command could not start") from None
    try:
        try:
            status = process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            raise FixtureError("standalone build or consumer command timed out; owned processes stopped") from None
        if session_groups(process.pid):
            raise FixtureError("standalone command exited with running children")
        if status:
            raise FixtureError(f"standalone build or consumer command failed (exit {status})")
    finally:
        # Includes Ctrl-C/SystemExit and exceptions during post-exit observation.
        # A cleanup failure replaces the outcome; it never claims children stopped.
        stop_command(process)


class Server:
    def __init__(self, binary, config, users, environment, directory, port=0):
        self.log = open(directory / "server.stderr", "w+")
        self.process = subprocess.Popen(
            [str(binary), "serve", "--postgres-config", str(config), "--users", str(users),
             "--host", "127.0.0.1", "--port", str(port)],
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=self.log,
            env=environment, text=True, cwd=directory)
        self.address = None

    def ready(self):
        lines = queue.Queue()
        threading.Thread(target=lambda: lines.put(self.process.stdout.readline()), daemon=True).start()
        try:
            line = lines.get(timeout=45).strip()
        except queue.Empty:
            raise FixtureError("standalone listener readiness timed out") from None
        prefix = "Listening on 127.0.0.1:"
        if not line.startswith(prefix) or not line[len(prefix):].isdigit():
            raise FixtureError("standalone process failed before listener readiness")
        self.address = ("127.0.0.1", int(line[len(prefix):]))
        return f"http://{self.address[0]}:{self.address[1]}/api"

    def stop(self, selected_signal):
        self.process.send_signal(selected_signal)
        try:
            status = self.process.wait(timeout=45)
        except subprocess.TimeoutExpired:
            raise FixtureError("standalone graceful signal shutdown timed out") from None
        if status != 0:
            raise FixtureError("standalone graceful signal shutdown failed")
        with socket.socket() as probe:
            probe.settimeout(2)
            if probe.connect_ex(self.address) == 0:
                raise FixtureError("standalone listener survived process shutdown")
        with socket.socket() as rebound:
            rebound.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            rebound.bind(self.address)

    def close(self):
        # Failure cleanup owns only this exact child; it never signals by name.
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=45)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=10)
        self.process.stdout.close()
        self.log.close()


def wait_for_database_cleanup(fixtures, schema):
    deadline = time.monotonic() + 10
    while True:
        # schema is generated below from ASCII hex, never supplied by a caller.
        counts = fixtures.command(fixtures.containers[0],
            "SELECT count(*),count(*) FILTER(WHERE xact_start IS NOT NULL) "
            "FROM pg_stat_activity WHERE datname=current_database() "
            f"AND application_name='rusty-haystack:{schema}'")
        sessions, transactions = map(int, counts.split("|"))
        if sessions == transactions == 0:
            return
        if time.monotonic() >= deadline:
            raise FixtureError("standalone PostgreSQL session ownership check failed")
        time.sleep(0.1)


def qualify(fixtures, fixture, toolchain):
    directory = fixtures.directory
    installation = directory / "installation"
    cargo = ["cargo", f"+{toolchain}"]
    # A real Cargo installation into an initially absent consumer directory.
    # Both build target and install root are initially absent. This is not
    # release packaging; only this disposable build cache is removed afterward.
    checked(cargo + ["install", "--locked", "--path", "haystack-cli", "--features", "postgres",
                     "--debug", "--root", str(installation), "--target-dir", str(directory / "build")])
    binary = installation / "bin" / "haystack"
    if not binary.is_file():
        raise FixtureError("installed standalone executable is absent")
    checked(cargo + ["test", "--locked", "-p", "rusty-haystack-cli", "--features", "postgres",
                     "--test", "standalone_postgres", "--no-run"])
    connection = json.loads(fixture.read_text())
    schema = f"standalone_{fixtures.token}"
    config = directory / "postgres.toml"
    # JSON strings are also valid TOML basic strings for these generated inputs.
    fields = {"version": 1, "host": connection["tls_server_name"], "port": connection["port"],
              "database": connection["database"], "username": connection["user"],
              "schema": schema, "password_env": "HAYSTACK_POSTGRES_PASSWORD"}
    private_write(config, "\n".join(f"{key} = {json.dumps(value)}" for key, value in fields.items())
                  + '\n[transport]\nmode = "verify-full"\nadditional_ca_file = "ca.crt"\n')
    users = directory / "users.toml"
    environment = os.environ.copy()
    environment["HAYSTACK_POSTGRES_PASSWORD"] = connection["password"]
    environment["HAYSTACK_PASSWORD"] = secrets.token_urlsafe(32)
    # Pre-create private: the product user command preserves this file's mode.
    private_write(users, "[users]\n")
    for username, permissions in (("operator", "read,write"), ("viewer", "read")):
        checked([str(binary), "user", "add", "--file", str(users), username,
                 "--permissions", permissions], environment, timeout=30)
    environment["HAYSTACK_STANDALONE_PASSWORD"] = environment.pop("HAYSTACK_PASSWORD")
    environment["HAYSTACK_STANDALONE_STATE"] = str(directory)

    # A failed listener may have initialized schema metadata; it must still
    # release every database session, report failure and never announce ready.
    with socket.socket() as blocker:
        blocker.bind(("127.0.0.1", 0))
        blocker.listen()
        server = Server(binary, config, users, environment, directory, blocker.getsockname()[1])
        try:
            try:
                status = server.process.wait(timeout=45)
            except subprocess.TimeoutExpired:
                raise FixtureError("occupied-listener startup did not terminate") from None
            if status == 0 or "Listening on" in server.process.stdout.read():
                raise FixtureError("occupied-listener startup did not fail closed")
            wait_for_database_cleanup(fixtures, schema)
        finally:
            server.close()
    print("Occupied listener failed without readiness; database sessions released", flush=True)

    for phase, selected_signal in (("populate", signal.SIGTERM), ("recover", signal.SIGINT)):
        server = Server(binary, config, users, environment, directory)
        try:
            environment["HAYSTACK_STANDALONE_URL"] = server.ready()
            # OwnedTransaction closes physical sessions after every transaction
            # and min_connections is zero. Readiness does not promise an idle
            # session; successful durable HTTP work below proves DB use.
            environment["HAYSTACK_STANDALONE_PHASE"] = phase
            checked(cargo + ["nextest", "run", "--locked", "-p", "rusty-haystack-cli", "--features", "postgres",
                             "--test", "standalone_postgres", "-E", "test(=installed_standalone_authorized_workflow)",
                             "--run-ignored", "ignored-only", "--no-tests", "fail", "--test-threads", "1"], environment, timeout=120)
            server.stop(selected_signal)
            wait_for_database_cleanup(fixtures, schema)
            print(f"Standalone {phase}: authorized HTTP flow passed; signal exit 0, listener and database sessions released", flush=True)
        finally:
            server.close()

    # The same installed executable publishes an archive, ends that maintenance
    # operation, and reuses the unchanged file for a fresh restore intent.
    control = f"control_{fixtures.token}"
    target = f"restored_{fixtures.token}"
    unused = f"unused_{fixtures.token}"
    selector = directory / "selector.toml"
    selected_fields = {key: value for key, value in fields.items() if key != "schema"}
    selected_fields["selector_schema"] = control
    private_write(selector, "\n".join(f"{key} = {json.dumps(value)}" for key, value in selected_fields.items())
                  + '\n[transport]\nmode = "verify-full"\nadditional_ca_file = "ca.crt"\n')
    archive = directory / "backup.haystack"
    admin = [str(binary), "storage"]
    export = admin + ["export", "--config", str(selector), "--operation", "archive",
                      "--source", schema, "--target", unused, "--output", str(archive)]
    checked(export, environment, timeout=60)
    checked(export, environment, timeout=60)  # Identical no-clobber publication retry.
    checked(admin + ["verify", "--backup", str(archive)], environment, timeout=30)
    checked(admin + ["cancel", "--config", str(selector), "--operation", "archive"], environment, timeout=60)
    checked(admin + ["restore", "--config", str(selector), "--operation", "restore",
                     "--target", target, "--backup", str(archive)], environment, timeout=60)
    target_config = directory / "target.toml"
    private_write(target_config, config.read_text().replace(f'schema = "{schema}"', f'schema = "{target}"'))
    for rejected_config in (config, target_config):
        rejected = Server(binary, rejected_config, users, environment, directory)
        try:
            if rejected.process.wait(timeout=30) == 0 or "Listening on" in rejected.process.stdout.read():
                raise FixtureError("quiesced source or staged target became serveable")
        finally:
            rejected.close()
    checked(admin + ["activate", "--config", str(selector), "--operation", "restore"], environment, timeout=60)
    checked(admin + ["reconcile", "--config", str(selector), "--operation", "restore"], environment, timeout=30)
    selected = Server(binary, selector, users, environment, directory)
    try:
        environment["HAYSTACK_STANDALONE_URL"] = selected.ready()
        checked(cargo + ["nextest", "run", "--locked", "-p", "rusty-haystack-cli", "--features", "postgres",
                         "--test", "standalone_postgres", "-E", "test(=installed_standalone_restore_selected_authority)",
                         "--run-ignored", "ignored-only", "--no-tests", "fail", "--test-threads", "1"], environment, timeout=120)
        selected.stop(signal.SIGTERM)
        for owned_schema in (schema, target, control):
            wait_for_database_cleanup(fixtures, owned_schema)
        print("Standalone archived restore: selector served fresh authority, exact retained data and invalid old receipts; sessions released", flush=True)
    finally:
        selected.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--docker", action="store_true", required=True,
                        help="create and remove a uniquely owned PostgreSQL 18.6 TLS fixture")
    parser.add_argument("--toolchain", default="1.99.1")
    args = parser.parse_args()
    fixtures = Fixtures()
    result = 1
    try:
        fixtures.certificates()
        fixture = fixtures.start()
        qualify(fixtures, fixture, args.toolchain)
        result = 0
    except FixtureError as error:
        print(f"Standalone PostgreSQL qualification failed: {error}", file=sys.stderr)
    except (OSError, subprocess.SubprocessError, ValueError):
        # Native diagnostic text, private files and environment remain private.
        print("Standalone PostgreSQL qualification failed; no successful qualification claimed", file=sys.stderr)
    finally:
        try:
            fixtures.close()
        except (FixtureError, OSError):
            print("Task-owned standalone fixture cleanup incomplete", file=sys.stderr)
            result = 1
    return result


if __name__ == "__main__":
    sys.exit(main())
