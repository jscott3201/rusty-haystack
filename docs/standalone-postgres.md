# Standalone core and PostgreSQL workflow

The `postgres` CLI feature serves one durable dataset through the existing HTTP
API and SCRAM authentication. It selects the bundled pinned HTTP catalog,
bounded entity mutations, and bounded live history. The ordinary CLI build
remains a process-memory server and does not enable SQLx.

## Run the complete disposable recipe

From a checkout on a POSIX host with `ps`, Rust 1.99.1, cargo-nextest 0.9.146,
Python 3, Docker and OpenSSL available:

```sh
python3 scripts/postgres/standalone.py --docker
```

This is a runnable consumer recipe, including writes and history. It installs
`haystack` into a new temporary installation using locked Cargo dependencies and
`--features postgres --debug` and a fresh temporary build target, starts that
installed binary from outside the checkout, and supplies explicit private configuration and generated credentials.
The debug installation is a local build/install qualification, not a release
artifact or production deployment qualification. The temporary build target is
removed with the fixture; existing repository build caches are retained. A clean
checkout needs no existing `target`, user file, database,
environment password, Python environment, or private workstation path.

The recipe creates one uniquely named PostgreSQL 18.6 TLS container and volume
using the same pinned image and resource bounds as the
[PostgreSQL authority qualification](postgres-durability.md). It verifies the
server's version and durability settings. The following checks must all pass:

1. An occupied HTTP port causes a nonzero startup exit without a `Listening on`
   banner, and all database sessions owned by that application disappear.
2. The installed server announces its actual loopback address only after
   application readiness. Database use is demonstrated by the durable operations
   below; idle sessions may be zero because each transaction retires its connection.
3. A real SCRAM operator login succeeds; an unauthenticated protected request
   receives HTTP 401 and an incorrect password fails authentication. A viewer
   can read, but receives HTTP 403 and an error grid for entity and history
   submissions; independent subsequent reads show no effect.
4. The operator creates `standalone_point`, a Number history point with UTC
   timezone and Celsius units, then writes samples `20.5°C` and `21.5°C` at
   `2026-01-01T00:00:00Z` and `2026-01-01T00:01:00Z`. Both responses must be
   decoded committed receipts with PostgreSQL durability qualification.
5. SIGTERM yields exit status zero. The listener refuses new connections, its
   port can be rebound, and PostgreSQL reports zero sessions and transactions
   with that application's name.
6. A new process recovers the same configuration and schema. Fresh SCRAM logins
   read the same entity, samples, dataset/incarnation/revision and history
   authority/incarnation/generation. Original operation identities reconcile
   their retained committed receipts without resubmitting the writes.
7. SIGINT produces the same successful cleanup evidence. The runner removes
   its installation, private files, container and volume, including on failure.
   Build and consumer commands run in owned process sessions. On timeout or Ctrl-C, the runner first requests graceful cancellation, then applies
   bounded TERM/KILL fallback to remaining groups in that session, including
   nextest tests in separate groups. It reaps its direct child and waits for all
   session members to stop; an incomplete observation or cleanup fails the recipe.

The public Rust client calls implementing this sequence are in
[`standalone_postgres.rs`](../haystack-cli/tests/standalone_postgres.rs).
The runner executes its `populate` and `recover` phases against the real process;
calling the ignored test without these explicit inputs fails. It uses
`entity_changes`, `submit_entities`, `reconcile_entity`, `his_read_scoped`,
`his_write_scoped`, and `reconcile_history`. Typed mutation outcomes must be
inspected: HTTP success alone does not mean a commit, and a transport failure
preserves an unknown effect rather than authorizing replay.

## Install and configure your own local instance

Build/install from the repository, with an explicit installation directory:

```sh
cargo +1.99.1 install --locked --path haystack-cli --features postgres \
  --root "$PWD/standalone-install"
./standalone-install/bin/haystack serve --help
```

This command uses Cargo's release build profile; the disposable recipe above
uses the faster debug profile. Neither command enables unrelated optional
integration profiles. The executable includes its pinned catalog, so serving
does not depend on the current working directory or external catalog files.

For a disposable PostgreSQL instance already listening at `127.0.0.1:55432`,
with database and login both named `haystack_local`, create `postgres.toml`:

```toml
version = 1
host = "127.0.0.1"
port = 55432
database = "haystack_local"
username = "haystack_local"
schema = "standalone"
password_env = "HAYSTACK_POSTGRES_PASSWORD"

[transport]
mode = "loopback-plaintext"
```

Every top-level field shown is required. Set the named password variable to the
password you provisioned for that database; it must be nonempty Unicode. There
is no inline password or connection URL option, passfile lookup, implicit default
connection, or opportunistic TLS downgrade. Ambient `PG*` connection settings
listed in [PostgreSQL configuration](postgres-durability.md#select-and-own-the-application)
are rejected, even when empty. Unknown fields, malformed TOML, missing password
variables, and invalid authority configuration fail nonzero with bounded errors
that omit configuration source and credentials.

`loopback-plaintext` is an explicit development selection restricted to
`localhost` or literal loopback IPs. To require verified TLS, replace the
transport table with:

```toml
[transport]
mode = "verify-full"
additional_ca_file = "ca.crt"
```

`additional_ca_file` is optional; verification always uses bundled WebPKI roots
and also uses the additional CA when supplied. Relative CA paths resolve from
the configuration file's directory. The certificate must cover the configured
`host`. Configuration files are limited to 64 KiB and CA files to 1 MiB. The
application's finite default limits remain selected; this CLI profile does not
expose arbitrary resource-limit tuning.

Use an explicit SCRAM user file. `HAYSTACK_PASSWORD` below is the HTTP user's
password, independent of the PostgreSQL password variable. Set it in the current
shell to your chosen local user password, then run:

```sh
umask 077
printf '[users]\n' > users.toml
./standalone-install/bin/haystack user add operator \
  --file users.toml --permissions read,write
./standalone-install/bin/haystack serve \
  --postgres-config postgres.toml --users users.toml \
  --host 127.0.0.1 --port 8080
```

The server reads the hashed credentials from `users.toml`; it does not read
`HAYSTACK_PASSWORD`. Missing or empty user files are refused for PostgreSQL
serving. Authenticated `read` permits dataset and catalog reads; `write` together with `read` permits
entity/history mutation and receipt reconciliation. The `admin` permission
satisfies both, matching the server's existing permission semantics. There is
no per-entity tenant partitioning in this coarse-permission profile.

In another shell with that HTTP password set as `HAYSTACK_PASSWORD`, the existing
CLI can perform a read:

```sh
./standalone-install/bin/haystack client read \
  --url http://127.0.0.1:8080/api --username operator 'point'
```

The complete write/history example is the disposable recipe above. The current
CLI `client his-read` command sends the legacy history request; it does **not**
select this server's bounded history protocol. Use the public scoped client
methods demonstrated in the recipe for history and mutation operations.

## Startup, restart and shutdown contract

An empty selected schema is initialized; an existing compatible schema is
recovered. `--file` and `--demo` conflict with `--postgres-config`: startup never
replaces persisted entities with a seed file or demo. Catalog replacement is
unavailable. Keep the same supported stored format and pinned catalog selection
when restarting. A mismatched catalog, incompatible schema, recovery limit,
connection failure or invalid configuration fails before readiness.

Database initialization can create schema metadata before a later HTTP bind
failure. A failed startup closes database resources but does not delete that
metadata. Stop with Ctrl-C or, on Unix, SIGTERM. The CLI seals new admission,
awaits owner close and actual termination, and only then drops its runtime.
Cleanup errors or a close timeout are nonzero failures; a timeout still waits
for actual owned work to finish and is not a successful cleanup receipt.

A default build still recognizes `--postgres-config` and gives a nonzero error
with the required `--features postgres` rebuild instruction. It never silently
falls back to memory. Ordinary command/configuration errors use stderr, while
the readiness address is printed on stdout.

This selected standalone workflow is distinct from the application-level
[durability profile](postgres-durability.md). History remains bounded live;
entity and history writes have separate transaction domains. No optional
analytics, Python/MCP integration, shared-instance serving recipe, catalog
activation, backup, migration tooling, power-loss recovery, production security
or release-artifact qualification follows from these checks. The required
Ubuntu PostgreSQL CI job runs both recipes; a local run establishes only its
actual host and revision. See the [support matrix](support-matrix.md).

## Recoverable backup and selection

The same installed executable supports `haystack storage` and version-1
`selector_schema` configuration. See [PostgreSQL backup and authority selection](postgres-recovery.md)
for maintenance windows, canonical archives, reusable restore intents and
interruption recovery. The disposable standalone runner also checks archived
restore through a selector, fresh authority identities and rejection of old receipts.
