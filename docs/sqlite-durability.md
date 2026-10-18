# SQLite entity authority

The optional `sqlite` feature of `rusty-haystack-app` selects one local SQLite file
as the durable authority for entity mutations and their retained change feed. It
uses the same authorized Add/Patch/Remove, expected-revision, operation-identity
and reconciliation contracts as [entity mutations](entity-mutations.md). A
committed receipt has the qualification `sqliteDurable`.

This profile selects no history service or history mutation service. It provides
no SQLite CLI selector, Python wheel, multi-instance writer coordination,
backup/restore transfer, migration, or network-filesystem qualification. Those
require separate contracts and evidence. Core and default application features
remain empty; SQLite and PostgreSQL can be built independently or together.

## Select and own one file

Enable `rusty-haystack-app` with `features = ["sqlite"]`, then construct the
application before starting listeners:

```rust,ignore
use haystack_app::{AllowAll, AllowAllMutations};
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
    },
    SqliteServiceLimits::default(),
).await?;
let owner = builder.start(&tokio::runtime::Handle::current())?;
owner.ready().await?;
let mutations = owner.handle().mutation_service().expect("SQLite entity selection");
// Submit through mutations; reads use owner.handle().read_service().
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

The schema has versioned, strictly checked tables for metadata, entities, entity
changes and receipts. Metadata retains dataset, graph incarnation, catalog
selection, revision, feed floor and cursor authentication key. Unsigned revisions
are fixed eight-byte big-endian BLOBs, avoiding signed INTEGER and floating-point
conversion. Payloads retain canonical typed values, including integer/float
distinctions, float bits and NaNs, units, reference display, DateTime timezone,
buffers, nominal provenance and nested absent/null/none distinctions.

The constructor rejects unknown schema objects, format/catalog mismatches,
missing metadata, malformed payloads and configured recovery/receipt limits.
It counts retained rows and encoded bytes before materializing their payloads.
An existing damaged authority is never treated as an empty database. There is no
automatic format migration. Reads verify the selected metadata and durability
configuration before using the recovered projection.

Recovery and mutation use finite limits from `SqliteLimits`, `ReadLimits` and
`MutationLimits`. The initial recovery timeout defaults to five seconds and covers native reads,
Rust decoding/restoration, service construction and successful builder delivery; cleanup
has a separate two-second allowance. Request work uses its admitted absolute
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
before COMMIT. This initial profile replaces the bounded stored projection on
each mutation; it is not an unbounded or measured high-throughput store.

The publication gate remains held until the recovered in-memory projection and
its notification are published. Concurrent reads, retries and receipt queries
cannot acknowledge a committed SQL receipt before that publication. A failed
publication seals admission and requires reopening the authority.

An ambiguous transaction/acknowledgement failure is resolved only through a new
write-serialization barrier and receipt read. If the durable record proves
commit, publication completes and returns `Committed`. If it proves the owning
attempt remained pending after rollback, that publisher can persist `Rejected` when its encoded outcome fits the receipt
byte cap. If settlement would exceed that cap, the durable `Pending` binding is
retained so the same configuration can reopen it.
If proof is unavailable, the result stays `Unknown`; an error alone never proves
rejection. A crash before the final COMMIT leaves the durable pending binding and
no entity/revision/feed effects. Recovery retains it as `Pending`; observers and
identical retries do not replay or automatically settle it. A crash after COMMIT
recovers the final receipt and complete effects without resubmission.

The stored cursor key permits retained feed continuation after close/reopen.
Receipt capacity is bounded and receipts are not automatically evicted or
rebound. Changing authorization is checked against retained original intent
before revealing a receipt. History and entity mutations do not share a SQLite
commit in this profile because history is not selected.

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
publication ordering. Separate child processes are killed at native transaction
boundaries before and after COMMIT. This is process-crash evidence, not simulated
power loss or media-durability qualification. The child helper is invoked by its
parent scenarios, not counted as a standalone qualification scenario.

Hosted qualification selects Linux x86_64; the same native suite can run locally
on macOS arm64. A configured workflow is not evidence of its execution at any
particular revision. The combined-feature run excludes ignored PostgreSQL fixture
tests by default; [PostgreSQL native qualification](postgres-durability.md) remains
a separate required selection when shared storage code changes.
