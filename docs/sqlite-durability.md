# SQLite entity and history authorities

The optional `sqlite` feature of `rusty-haystack-app` selects one local SQLite file
as the durable authority for entity mutations, their retained change feed, and
bounded history reads and one-point timestamp upserts. It uses the same
authorized contracts as [entity mutations](entity-mutations.md) and
[history mutations](history-mutations.md). Both domains qualify committed
receipts as `sqliteDurable`; their identities, counters and atomic units remain
independent.

This profile selects both a history service and a history mutation service. It
provides no SQLite CLI selector, Python wheel, multi-instance writer coordination,
backup/restore transfer, migration, or network-filesystem qualification. Those
require separate contracts and evidence. Core and default application features
remain empty; SQLite and PostgreSQL can be built independently or together.

## Select and own one file

Enable `rusty-haystack-app` with `features = ["sqlite"]`, then construct the
application before starting listeners:

```rust,ignore
use haystack_app::{AllowAll, AllowAllMutations, AllowAllHistoryMutations};
use haystack_app::sqlite::{self, SqliteConfig, SqlitePolicies, SqliteServiceLimits};
use haystack_core::xeto::catalog::Catalog;
use std::sync::Arc;

// Explicit trusted embedding policies; ordinary read permission grants no write access.
let builder = sqlite::application(
    SqliteConfig::new("/absolute/local/directory/entities.db"),
    Catalog::load_http_pinned()?,
    SqlitePolicies {
        reads: Arc::new(AllowAll),
        entities: Arc::new(AllowAllMutations),
        history: Arc::new(AllowAllHistoryMutations),
    },
    SqliteServiceLimits::default(),
).await?;
let owner = builder.start(&tokio::runtime::Handle::current())?;
owner.ready().await?;
let mutations = owner.handle().mutation_service().expect("SQLite entity selection");
let history = owner.handle().history_mutation_service().expect("SQLite history selection");
// Submit through mutations/history; their shared read services retain admission.
owner.close().await?;
let termination = owner.terminated().await;
assert!(termination.cleanup_errors.is_empty());
```

The parent directory must exist and the configured path must be absolute. The
constructor canonicalizes the path and exclusively locks a persistent
`entities.db.owner.lock` sidecar with the operating system file-lock API.
Participating owners using a symbolic-link alias resolve to the same lock; files
with multiple hard links are rejected. The lock file is not unlinked on close,
because unlinking it could let two processes lock different inodes. Stale file
contents carry no ownership; the open locked handle does.

The application retains that handle through physical native connection close.
A second owner receives `AlreadyOwned`, including while the first builder is
unstarted or cleanup remains active. Dropping an unstarted builder or abandoning
construction schedules/retains actual native cleanup. Startup failure closes the
connection before releasing ownership. A running application's normal close
waits for admitted work, rolls back any active transaction and explicitly closes
the connection. Public errors omit SQL, driver diagnostics and filesystem paths.

Advisory locking coordinates participating application owners. External SQL
writes, lock-sidecar removal, filesystem replacement and network filesystems are
outside this profile. Qualification temporarily holds an external SQLite write
lock to exercise contention; it does not authorize external data mutation.
Neither the read service nor mutation store exposes a mutable native graph.
The selected history store rejects native `write` and `reset_point` bypasses.
The borrowed history service shares the application's one database resource;
closing it cannot prematurely close the entity authority.

## Native durability and recovery

The exact optional dependency is rusqlite **0.40.2**, using its bundled
libsqlite3-sys **0.38.2** / SQLite **3.53.2**. First-party code uses safe APIs; the
existing `unsafe_code = "forbid"` workspace policy remains in force. SQLite's C
implementation and its dependency bindings are an explicitly selected native
dependency, not first-party unsafe code.

Construction sets and reads back `journal_mode=WAL`, `synchronous=FULL`,
`foreign_keys=ON`, `busy_timeout=5`, `fullfsync=ON` and
`checkpoint_fullfsync=ON`. Defensive mode is enabled and trusted schema is
disabled. A successful readback establishes the native configuration; it does
not prove a filesystem, controller or device honored a power-loss flush.

The format 2 schema has strictly checked tables for metadata, entities, entity
changes and receipts, plus history points, samples, changes and receipts.
Metadata retains dataset, graph incarnation, catalog
selection, revision, feed floor and cursor authentication key, and an independent
history authority and global history sequence. Unsigned revisions, point
generations and history sequences are fixed eight-byte big-endian BLOBs, avoiding
signed INTEGER and floating-point conversion. Payloads retain canonical typed
entity values, including integer/float
distinctions, float bits and NaNs, units, reference display, DateTime timezone,
buffers, nominal provenance and nested absent/null/none distinctions.

The constructor rejects unknown schema objects, format/catalog mismatches,
missing metadata, malformed payloads and configured recovery/receipt limits.
It counts retained rows and encoded bytes before materializing their payloads.
An existing damaged authority is never treated as an empty database. There is no
automatic format migration: legacy entity-only format 1 and unknown future
formats are rejected before journal-mode or authoritative-data changes. Version
rejection qualification uses an independently constructed populated format 1
file and checks its schema, metadata, rows, receipts, feed and journal mode remain
unchanged. Reads verify the selected metadata and durability configuration before
using the recovered projection.

Recovery and mutation use finite limits from `SqliteLimits`, `ReadLimits` and
`MutationLimits`, plus `HistoryLimits`, `HistoryMutationLimits` and
`HistoryStoreLimits`. The initial recovery timeout defaults to five seconds and
covers native reads, Rust decoding/restoration, service construction and
successful builder delivery; cleanup has a separate two-second allowance.
Request work uses its admitted absolute
deadline and cancellation tokens. SQLite busy waits have a five-millisecond
quantum; a connection-scoped progress handler interrupts active SQL without a
detached interrupt racing a later request. Native blocking work retains its
admission/lifecycle lease after an observer times out or cancels. Read verification
continues an already admitted transport guard during graceful drain, retaining
its deadline, caller/session cancellation and owner-stop fence. Admission sealing
still denies new work and fences mutation publication.

## Receipts and atomic publication

Before effects, a separate transaction durably binds a `Pending` receipt to the
principal kind/subject, dataset, incarnation, operation ID and canonical request.
The final transaction atomically updates entities, graph revision, retained
changes and final receipt. Bounded graph reconstruction and serialization happen
before COMMIT. Entity publication replaces the bounded stored entity projection
on each mutation; it is not an unbounded or measured high-throughput store.

The publication gate remains held until the recovered in-memory projection and
its notification are published. Concurrent reads, retries and receipt queries
cannot acknowledge a committed SQL receipt before that publication. A failed
publication seals admission and requires reopening the authority.

An ambiguous transaction/acknowledgement failure is resolved only through a new
write-serialization barrier and receipt read. If the durable record proves
commit, publication completes and returns `Committed`. If it proves the owning
attempt remained pending after rollback, that publisher can persist `Rejected`
when its encoded outcome fits the receipt byte cap. If settlement would exceed
that cap, the durable `Pending` binding is retained so the same configuration can
reopen it. If proof is unavailable, the result stays `Unknown`; an error alone never proves
rejection. A crash before the final COMMIT leaves the durable pending binding and
no entity/revision/feed effects. Recovery retains it as `Pending`; observers and
identical retries do not replay or automatically settle it. A crash after COMMIT
recovers the final receipt and complete effects without resubmission.

The stored cursor key permits retained feed continuation after close/reopen.
Receipt capacity is bounded and receipts are not automatically evicted or
rebound. Changing authorization is checked against retained original intent
before revealing a receipt. Entity and history mutations never share an atomic
commit or operation-receipt namespace.

## Bounded history contract

Each history write targets one point and binds its exact original request order,
expected generation, timestamp timezone spelling and principal. A stable history
authority and deterministic per-point incarnation make observing an empty point
at generation zero repeatable after reopen, even before its first write. The last
submitted sample at a duplicate instant wins. Every admitted nonempty batch
advances generation once, including identical-value upserts. Empty batches retain
`Rejected(EmptyBatch)` without advancing samples, generation or history sequence.

After the separate Pending reservation, one transaction updates that point's
retained samples, generation and eviction watermark, appends/prunes the bounded
history change ledger, advances the global history sequence and stores the final
receipt. Prepared graph, point and policy dependencies are rechecked under the
same writer gate as entity publication. No graph/point guard spans native SQL.
The native commit is followed by projection/ledger/receipt publication before
acknowledgement or duplicate/reconcile lookup can cross that gate. Unprovable
outcomes seal history reads, including already opened pulls, until reopen.

Supported Number values retain exact bits and unit spelling, including signed
zero and the canonical unitless NaN; unsupported NaN payloads remain rejected by
the shared contract. Bool, Str and NA retain their distinction. Timestamps retain
nanoseconds and fixed offsets, while original request timezone spelling remains
part of receipt binding. Admitted minute leap seconds preserve Chrono's excess
nanosecond field through native storage and reopen; excess nanoseconds away from
minute end remain invalid. Number points require a declared known unit. Existing
half-open ranges and timezone/DST interpretation apply without a snapshot promise.
Recovery validates SQL seconds/nanoseconds against canonical sample payloads,
point state against the retained ledger, and retained changes against their final
receipts. Every persisted point's current generation and retained count also need
a committed receipt proving that tip, even after its change leaves the bounded
ledger. Malformed or contradictory state is rejected, never silently reset.

The source remains demand-driven `BOUNDED_LIVE`. Each pull checks its point's
generation and admitted cancellation/deadline; a changed generation terminates
the session before copying another sample. Source open/pull operations recheck
the durable authority fence through lock waits, copying and worker delivery, so
an in-flight operation cannot succeed past an observed backend seal.
Source byte/work limits apply before
cloning a row. No SQL transaction or database cursor is held across consumer
pulls. History writes do not advance entity revisions or invalidate retained
entity feed cursors.

`SqliteLimits` defaults to 100,000 global retained history samples, 64 MiB of
encoded point/sample/change data and 64,000,000 units of bounded history work.
Work accounting is `4 * (data_bytes + receipt_bytes) + 128 * retained_rows`.
`max_receipts` and `max_receipt_bytes` apply independently
to entity and history domains. History store limits additionally bound points,
per-point sample retention, receipt count/retained memory and change capacity.
The same native row/byte/work accounting applies before recovery allocation and
before every reservation, final commit and rejection settlement. A commit that
would exceed a bound rolls back. If its rejection settlement also cannot fit,
the bounded durable Pending receipt remains. Receipt capacity is never reclaimed
by silently forgetting an operation identity. Individual native
history BLOB fields are checked while borrowed before any owned copy.

## Validation

Run these explicit selections in addition to the repository's full local gate:

```sh
cargo nextest run --locked -p rusty-haystack-app --no-default-features --features sqlite
cargo test --locked -p rusty-haystack-app --no-default-features --features sqlite --doc
cargo clippy --locked -p rusty-haystack-app --no-default-features --features sqlite --all-targets -- -D warnings
cargo clippy --locked -p rusty-haystack-app --no-default-features --features postgres,sqlite --all-targets -- -D warnings
cargo nextest run --locked -p rusty-haystack-app --no-default-features --features postgres,sqlite
python3 -m unittest discover -s scripts/sqlite -p 'test_*.py' -v
cargo nextest run --locked -p rusty-haystack-app --features sqlite -E 'test(sqlite::qualification) & not test(process_crash_worker)' --success-output immediate
```

The selected native suite uses disposable real local files and the bundled
engine. It prints the actual engine source ID and pragma readbacks and exercises
CRUD/reopen, stable identity/cursors, canonical payloads, native constraint
rollback, external write-lock bounds, cancellation/deadline cleanup, startup
failure, ownership aliases, malformed recovery, lost acknowledgements and
publication ordering. History cases also cover empty and identical batches,
exact values/DST ranges, expected-generation and principal binding, retention,
receipt growth, bounded live pulls, delayed dependency fences, corruption, full
u64 counter overflow and unchanged legacy-format rejection. The between-pull
reader check uses a bounded WAL truncate checkpoint, first demonstrating a busy
result with a deliberately pinned reader and then a complete checkpoint after
that reader releases. Separate child
processes are killed at native transaction boundaries before and after COMMIT.
This is process-crash evidence, not simulated
power loss or media-durability qualification. The child helper is invoked by its
parent scenarios, not counted as a standalone qualification scenario.

Hosted qualification selects Linux x86_64; the same native suite can run locally
on macOS arm64. A configured workflow is not evidence of its execution at any
particular revision. The combined-feature run excludes ignored PostgreSQL fixture
tests by default; [PostgreSQL native qualification](postgres-durability.md) remains
a separate required selection when shared storage code changes.
