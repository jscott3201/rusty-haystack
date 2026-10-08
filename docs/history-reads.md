# Bounded history reads

`haystack-app::HistoryService` is the common authorized single-point history
reader for embedding and scoped HTTP. Select it once on an `ApplicationBuilder`
using `owned_history(service)` or `borrowed_history(service)`. It must share that
builder's exact `ReadService`. Early application handles observe the selection.
Owned provider initialization runs before listeners, regardless of registration
order. Borrowed providers receive no initialization, rollback or close hooks.

```rust,no_run
use std::sync::Arc;
use haystack_app::{HistoryLimits, HistoryService, HisStore};
# fn select(builder: haystack_app::ApplicationBuilder) -> Result<haystack_app::ApplicationBuilder, haystack_app::ReadError> {
let store = Arc::new(HisStore::new());
let history = HistoryService::new(
    builder.handle().read_service(), store, HistoryLimits::default(),
)?;
let builder = builder.owned_history(history)?;
# Ok(builder)
# }
```

An embedding calls `open(context, HistoryReadRequest { id, range })`, then
`session.next().await`. Each call requests at most one batch. There is no
prefetch or whole-range snapshot. A cancelled wait for `next` can be resumed:
the session retains that same pending batch. Dropping or closing the session,
caller cancellation, an absolute deadline, or application stop triggers cleanup
without another pull. The application retains its original work registration
and admission capacity until an in-flight provider future and session cleanup
actually finish. A public shutdown timeout is not a termination receipt.
Providers must not detach untracked work from open, pull or close futures.

The selected memory provider holds a point handle and cursor and copies only
admitted batches. Global and per-principal session limits, source-sized batch
and total row/byte/work budgets, read-service value/retention budgets, and the
absolute request deadline are finite. Principal capacity keys distinguish
anonymous, authenticated and trusted embedding namespaces; changing permission
lists does not evade the same authenticated subject's limit. Response metadata
reports the selected batch, total, work and collector response bounds. Byte
accounting reserves structural overhead and worst-case H4 escaping; it is a
conservative source-size allowance, not a promise of a specific payload size.
Actual limits may stop a result below its row ceiling.

## Range, schema and identity

Ranges are start-inclusive and end-exclusive. Calendar dates include the final
fractional second and end at the following point-local midnight. In a date
pair, the final date is included through its following midnight. `today` and
`yesterday` use the point-local date; `HistoryClock` makes the current instant
injectable. A zero-width explicit DateTime range is empty and complete; a
reversed range is invalid. DST days can contain 23 or 25 hours. Ambiguous or
nonexistent calendar midnights produce an explicit validation error.

The point must declare `his`, `kind` and `tz`. H4 supports Bool, Number and Str
schemas, plus NA error samples. Values retain their original Kind: Int/Float,
Null/None/Remove, Ref, nominal and other rich values are unsupported by this
selected profile. Nothing is coerced, silently dropped, or replaced with Null.
A Number sample can be unitless or use the same registered unit identity as the
point. Unit spelling and numeric bits are preserved; unit-bearing NaN is
unsupported. A present non-string JSON v4 Number unit is a codec error.

Timezone handling uses the existing optional core chrono-tz implementation and
Haystack short-name table. The app and client opt into that feature; core's
default features remain empty. `Rel` and full IANA path spellings are not this
H4 profile. Valid explicit DateTime boundaries may use another admitted zone,
for example GMT for a New_York point; the input instant is preserved. Inconsistent
explicit offsets are rejected. Rows and `hisStart`/`hisEnd` use the point's actual
historical offset and exact admitted spelling. Range scalars preserve fractions.

The memory provider advertises demand-driven generation-checked live reads,
cooperative cancellation, and no snapshots or native-rich-value profile. Each
point has a history incarnation and checked generation under a provider history
authority. A nonempty native write advances only that point's generation;
subsequent pulls interrupt before mixing generations. Native duplicate timestamp
writes retain the last input value. Generation exhaustion rejects before effects.
Provider duplicate/out-of-order samples, incompatible schema, impossible coverage,
oversized batches and contradictory completion are failures.

Metadata separately observes the entity graph incarnation/revision, catalog
generation, an opaque policy observation, and history authority/incarnation/
generation. There is no atomic entity/history snapshot claim. Current operation,
point and schema/sample field policy runs before provider access and each
published batch; policy or graph/schema changes interrupt the session. Missing
and denied points have the same unavailable result before provider access.

Coverage reports retained first/last timestamps, retained count, and the largest
evicted timestamp. `Complete` covers retained records in the requested interval;
it never claims to reconstruct data removed by retention. The native memory
store retains at most one million items per point by default. Its trusted
`write` and inclusive-end, materializing `read` compatibility methods are outside
the authorized bounded service. History write authorization and receipts are
separate future work.

## HTTP and client contract

Scoped `hisRead` requires a `history` STR grid metadata marker containing the
strict typed-v1 `bounded-history-v1` request control. The shared core codec builds
it. Results retain ordinary H4 `ts`/`val` rows and `id`, `hisStart`, `hisEnd`, plus
a strict typed-v1 STR control with schema, bounds, independent observations,
coverage, returned count and terminal state. Public counters use canonical
unsigned decimal strings. This extension is project-owned, not a new official
H4 wire format or an Arrow/Parquet encoding.

This HTTP adapter collects **one bounded response**. It does not implement a
remote open/pull/close protocol or progressive HTTP streaming. Zinc and JSON
v3/v4 are admitted; unsupported negotiation fails before provider work. Only
selected history appears in scoped operation discovery; scoped `hisWrite`
remains disabled. External routers and owned listeners consume the exact
application selection, and reject independently configured providers.

`Complete`, `Limited`, `Interrupted` and `Failed` are explicit terminal outcomes.
Partial rows remain alongside their terminal state. The Rust
`HaystackClient::his_read_scoped` helper preserves them and binds the result to
the requested point/range, checking counts, order, schema, range and provenance
consistency. Missing/malformed/contradictory metadata is a protocol error. The
first-party HTTP transport bounds collection and does not automatically replay
or redirect the call. Custom `HistoryTransport` implementations explicitly own
that same transport contract.

The simple Rust `his_read`, Python and CLI grid helpers remain legacy calls.
They do not add the extension marker. A legacy server path using this provider
returns a plain grid only after complete bounded collection; a limited,
interrupted or failed collection is an explicit error, never an ordinary
partial grid. The server's standalone `start()` convenience method selects its
legacy provider through the same application ownership path.
