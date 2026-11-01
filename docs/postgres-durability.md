# PostgreSQL application authority

The optional `rusty-haystack-app/postgres` feature selects one bounded PostgreSQL
18.6 dataset for the existing authorized entity and history services. Default
builds continue to use explicit process-memory stores and do not enable SQLx.
Cooperating application processes may share one database/schema with the same
supported format and activated catalog. Managed admission observes a coherent
committed database cut; bounded periodic refresh supplies idle convergence.
This profile makes no general serializability, rolling mixed-version, geographic
replication, cross-server disaster recovery, power-loss, performance or production deployment claim.
For the bounded same-database backup/restore workflow, see
[backup and authority selection](postgres-recovery.md).

For an installed standalone server with explicit configuration and a disposable
authenticated HTTP write/history/restart recipe, see
[standalone PostgreSQL](standalone-postgres.md).

## Select and own the application

Call `haystack_app::postgres::application(config, catalog, policies, limits).await`
to recover a builder. `PostgresPolicies` separately supplies read, entity-write
and history-write authorization. Its selected services share the same managed
read admission and lifecycle. Add listeners/resources to the returned builder,
start it on the borrowed Tokio runtime, and await `owner.ready()` before use.
Database validation/recovery finishes before the builder is returned; readiness
also refreshes the complete projection before any selected listener serves.

Configuration uses explicit host, port, database, username, optional password,
schema and transport fields. There is no connection-URL or passfile constructor.
The environment must remain immutable during configuration. Presence of any of
`PGPORT`, `PGHOSTADDR`, `PGHOST`, `PGUSER`, `PGDATABASE`, `PGPASSWORD`,
`PGSSLROOTCERT`, `PGSSLCERT`, `PGSSLKEY`, `PGSSLMODE`, `PGAPPNAME` or `PGOPTIONS`
is rejected, including an empty or non-Unicode value. Public errors and Debug
omit driver diagnostics, endpoints, credentials and certificate material.

`PostgresTransport::VerifyFull` requires certificate-chain and hostname
verification, using bundled WebPKI roots plus an optional explicit CA. The
additional CA is not an exclusive trust pin. There is no plaintext fallback.
`LoopbackPlaintext` is a separate explicit development/test selection limited to
`localhost` and literal loopback addresses. Schema names pass a narrow ASCII
identifier allowlist and cannot be `public` or start with `pg_`.

The application requires PostgreSQL 18 or later, logged authority tables and
`fsync`, `synchronous_commit` and `full_page_writes` enabled. Runtime qualification
currently pins 18.6; the version check does not qualify every later release.
Every connection fixes statement, lock, transaction and idle-transaction limits,
`synchronous_commit=on` and the selected search path. Startup compares the stored
catalog selection with the supplied activated catalog. A mismatched catalog,
unsupported format, corrupt projection or exceeded recovery bound fails before
readiness. It never truncates stored data to make startup succeed.

The constructor owns a bounded background construction task. Dropping its future
cannot turn an unfinished driver operation into a cleanup receipt; failed
builder delivery awaits backend closure. Dropping an unstarted recovered builder
requests database cleanup. Keep the borrowed runtime driving. For a started
application, await `owner.close()` and inspect `owner.terminated()` for actual
work completion and cleanup errors, as in the [lifecycle contract](application-lifecycle.md).

## Authority and atomic publication

Initialization and supported format-1/2-to-format-3 transitions hold the schema's
exclusive advisory gate. Transactional DDL, format changes and relation checks
roll back together on failure. The gate uses the former exclusive-owner key, so
a live format-1 owner must close before transition. The format bump fences later
format-2 transactions; current format-3 transactions also require an active
authority state. Startup verifies logged
authority relations, required primary/foreign keys, the format constraint and
stored catalog/values; `IF NOT EXISTS` alone is not compatibility evidence.
Concurrent starters share one durable dataset, incarnation and cursor key.

Every supported writer and refresh takes the shared schema gate before locking
the singleton metadata row. Writers use explicit `READ COMMITTED READ WRITE`
and `FOR UPDATE`; a coherent refresh uses `READ COMMITTED READ WRITE` and
`FOR SHARE`, then reads entities, complete retained entity changes, history,
receipts and bounds through that one transaction. Every writer participates in
this order. The durable `owner_generation` column is now a stable authority
epoch; ordinary application startup does not increment it or fence healthy peers.

Managed admissions refresh under the original work lease and deadline. They
fail unavailable if the database cut cannot be verified; they never silently
serve an old projection as a successful fresh admission. A successful admission
witnesses a cut, and another process may commit afterward. Entity rows and
reference traversal use one graph observation. History remains bounded live.
Local commits and refresh publication share one writer gate, so a delayed cut
cannot overwrite a later local publication. A short graph capture fence covers
history-handle updates and graph replacement; SQL and pool waits occur outside
capture locks. Existing history sessions retain their point handles and detect
remote generation changes.

An application-owned task starts another bounded reconciliation attempt after
`PostgresLimits::refresh_interval` (250 ms by default). The interval bounds the
idle delay before an attempt, not total end-to-end freshness: acquisition,
transaction, publication contention and failure/retry add their configured
bounds. Quiescent reachable peers converge without a later write or wakeup.
There is no PostgreSQL LISTEN dependency; local notifications are disposable
hints, while durable heads, retained floors and complete changes govern replay.
Connection loss denies new work but permits bounded reconnect and fresh recovery.
An incompatible format/catalog/epoch or observed head regression seals the
instance and requires reconstruction. Every transaction also verifies dataset,
incarnation, catalog generation, history authority and cursor identity under the
metadata lock, so an already prepared write cannot cross an incompatible change. A failed local publication after proven
commit also fences service until reconstruction.

Only the application owns mutable projections. `ReadService::graph()` and
`EntityMutationStore::graph()` expose `GraphView`, a read-only inspection handle.
`native_graph()` returns `None` for this profile. Legacy native entity writes,
native history write/reset, simulated commands and hot catalog replacement are
unavailable rather than bypassing SQL authority. Ephemeral selections retain an
explicit native capability. Graph inspection is trusted local observation, not
a substitute for managed read authorization or a fresh database observation.

Entity commits serialize their full bounded graph/change projection, stable
identity, revision/floor and receipt in one SQL transaction. History commits
serialize the affected point's samples, generation, history sequence/change and
receipt in one SQL transaction. Preparation revalidates its durable entity graph,
catalog and history sequence dependencies under the metadata lock. A remote
advance yields an optimistic conflict without effects. Candidate graph indexes,
sample vectors and encoded payloads are prepared before COMMIT. Public projections and notifications
advance only after the durable receipt proves the commit. A rolled-back write
consumes no public entity revision or history sequence.

Operation IDs bind the stable principal kind/subject and exact original
canonical typed request. Retries with the same intent return the stored receipt;
changed intent conflicts. This includes original history ordering, duplicate
samples and DateTime offsets, even when normalized sample effects are equal.
A pending identity is durable and cannot be reused to replay an uncertain write.
The owning publication resolves ambiguous COMMIT only after a fresh metadata-row
serialization barrier and subsequent READ COMMITTED receipt query. A plain
absence lookup is not no-effect proof. Failure to complete the barrier remains
`Unknown`; a later admission must recover a fresh cut before serving work. An
observer's reconcile never rejects another process's live Pending preparation.
An orphan Pending identity remains bound and consumes finite receipt capacity;
this profile promises neither eventual cancellation nor automatic replay or
eviction. Before acknowledging a remotely committed receipt, the observing
process refreshes through its committed state. SQL commit followed by lost
in-memory publication is recoverable; it is never reported as `Rejected`.

Committed receipts use the explicit `PostgresDurable` / `postgresDurable`
qualification. That label refers to this selected store contract and required
PostgreSQL settings. It does not certify underlying hardware, operating-system
flush behavior, external storage configuration, backups or disaster recovery.

Canonical typed payloads remain authoritative. SQL IDs, unsigned revisions via
`numeric(20,0)`, and sample `(seconds,nanoseconds)` keys are checked projections.
Payloads preserve rich kinds, explicit null versus absence, exact integer and
float bits, units, nominal catalog/revision provenance and original temporal
representation. Recovery rebuilds indexes from bounded primary rows and validates
complete retained change units. Entity feed identity and signing key survive
restart; feed token expiry uses wall-clock time. Normal page cursors and active
snapshot sessions do not survive application restart.

## Bounds, reads and cleanup

`PostgresLimits` bounds pool capacity, acquisition/statement/lock/transaction and
snapshot duration and refresh interval, primary/change recovery rows/bytes/work, receipt count/bytes
and total history samples. Existing service-specific admission, work, retained
memory and history retention limits still apply. Oversized writes fail before
effects; recovery refuses oversized persisted state. Receipts are retained within
a finite capacity; this profile does not silently evict identity bindings.

Ordinary entity reads use the coherent application projection. Ordinary history
continues to advertise `HistoryCapabilities::BOUNDED_LIVE`; persistence does not
upgrade it to snapshot history or imply a cross-store read transaction.

For a fixed entity view, call `reads.postgres_snapshot(context).await` or transfer
an existing `ReadAdmission` into `postgres_snapshot()`. The returned
`PostgresSnapshot` owns one `REPEATABLE READ READ ONLY` transaction and bounded
recovered graph until close. It acquires a session-level shared schema gate on
its exact connection **before** `BEGIN`, preventing an RR snapshot frozen before
a pending migration. The gate remains held until actual physical closure.
`snapshot.read(request).await` uses the existing
filter, traversal, reference, policy, encoding and cursor engine against that
same view. Policy is refreshed on every page. Budgets accumulate across pages
under the original deadline and admission slot; callers cannot create a fresh
unbounded service from the snapshot. It covers entities only.

Explicitly await `snapshot.close()` for transaction, physical connection and work
registration cleanup. Dropping it or abandoning a page requests cancellation;
the worker retains its admission and pool capacity until cleanup finishes.
Owner shutdown observes that actual work. Transactions deliberately retire their
physical connection after acknowledged COMMIT/ROLLBACK in this bounded profile.
This prevents SQLx's queued Drop rollback/pool-return path from standing in for
completed cleanup. It has a connection setup cost and makes no throughput claim.
SQLx pools remain bounded; reusable build caches are unrelated to these resources.

## Subscription continuity and restore boundaries

State-subscription IDs, exact-session ownership and leases remain local to the
application process. Resume on a surviving owner refreshes and replays remote
changes from the durable retained feed, even after missed wakes or connectivity
loss. A retention gap returns explicit `Gap` resynchronization. Wrong-owner or
restarted-owner subscription IDs return authority/unknown resynchronization;
create a fresh authorized initial snapshot. This differs from the durable entity
feed cursor, whose key and position survive compatible application reconstruction.

The [backup/restore workflow](postgres-recovery.md) now implements explicit
coordinated authority changes between distinct schemas in one database. It
quiesces source work, validates canonical retained data, stages fresh identities
and atomically updates source/target states, selection and the operation journal.
Old instances fence on their next verified observation; prepared writes cannot
cross the epoch and retained resources require resynchronization. Archived
backups can be reused under a new restore intent after the producer was cancelled.
Observed same-epoch regression still fails rather than silently rewinding. Loss
of the whole database/control journal and cross-server disaster recovery remain
outside this profile.

## Explicit qualification

The baseline local gate remains unchanged. Run it and the selected PostgreSQL
profile separately. The profile requires Docker, OpenSSL, Rust 1.99.1 and
cargo-nextest 0.9.146:

```sh
python3 scripts/postgres/qualify.py --docker
```

The script creates two uniquely named loopback containers, an explicitly owned
PostgreSQL 18 volume and disposable TLS material using the pinned 18.6 image
index digest. It bounds container resources, verifies server settings, supplies
private fixture files, and removes only its own resources in `finally`. Existing
fixtures can be used without transferring their lifecycle:

```sh
python3 scripts/postgres/qualify.py \
  --fixture /absolute/private/tls.json \
  --plain-fixture /absolute/private/plain.json
```

Fixture JSON contains `host`, `port`, `database`, `user`, `password`,
`tls_server_name` and `root_certificate`. Keep it private; it is not a product
configuration or connection URL. The explicit test selection fails on missing
fixtures or an empty test inventory. Ignored runtime tests in a default build are
not qualification evidence.

The required Ubuntu `PostgreSQL 18.6 durability` CI job runs optional-feature
Clippy, app tests/doctests, deterministic fixture-startup checks, and the real
profile. The aggregate `CI OK` requires
that job to succeed. Tests cover restart and typed data, durable receipt binding,
real session termination before commit, discarded commit acknowledgment,
commit-before-publication recovery, unresolved barrier/lease retention,
shared epoch fencing, history retention/watermark restart, reservation-hit
reconciliation authorization, verified TLS and negative endpoints, bounded and
incompatible recovery, and snapshot/reference coherence and cleanup. A
one-connection pool case verifies cancelled and expired refresh/snapshot waiters,
actual lease/transaction cleanup, fresh capacity after releasing the holder, and
owner closure while a snapshot holds the pool and another refresh is waiting.

The shared-instance harness launches two distinct application OS processes on one
schema, exercising both service paths, reference reads, history-only idle catch-up,
foreign Pending receipts, reversed preparation/completion, real allocated-transaction
rollback with a peer observed waiting in PostgreSQL, refresh capture/publication
barriers, history dependency conflicts, subscription resume/gaps, process restart,
transactional migration rollback, after-gate snapshot format refusal, held-write
catalog fencing, explicit restore rotation and same-epoch regression. A transparent TCP relay cuts existing
connections and rejects new ones while the other process commits. Isolated
managed reads/feed/subscription work fails, then the same process reconnects and
catches up through periodic polling alone. The test records measured convergence
delay and configured interval; these fixture measurements are not a production SLA.

Discarded acknowledgment and held-commit scheduling use explicit client fault
seams over real SQL transactions. The TCP interruption case is actual local
connectivity evidence; neither it nor transaction rollback is power-loss or
geographic-network qualification. See the [support matrix](support-matrix.md)
for host and feature limits.
