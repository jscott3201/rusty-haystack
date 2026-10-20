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

Finite adapter collection uses `ReadAdmission::history_joined` with
`HistoryReadOptions` and the exact selected `HistoryService`. It tightens the
original row/output limits and absolute deadline, retains cumulative accounting,
and joins provider work on every result and error path before releasing
admission. Success waits without cancelling the token needed for encoding;
`HistoryReadSession::close()` continues to mean cancellation plus cleanup.
The MCP adapter retains its transport lease around this operation even if the
SDK abandons its handler. See [bounded MCP history](mcp-history.md).

The selected memory provider holds a point handle and cursor and copies only
admitted batches. Global and per-principal session limits, source-sized batch
and total row/byte/work budgets, read-service value/retention budgets, and the
absolute request deadline are finite. Principal capacity keys distinguish
anonymous, authenticated and trusted embedding namespaces; changing permission
lists does not evade the same authenticated subject's limit. Response metadata
reports the selected batch, total, work and collector response bounds. Byte
accounting reserves structural overhead and worst-case H4 escaping; it is a
conservative source-size allowance, not a promise of a specific payload size.
Actual limits may stop a result below its row ceiling. The HTTP collector shares
its cumulative budget and work lease through result-grid construction and final
encoding. Before provider copies it reserves metadata, original/grid copies,
codec temporary values, worst-case escaping and buffer growth. These conservative
reservations can return a partial `Limited` response under a small budget; they
are source-based accounting bounds, not allocator measurements. Absolute deadline
and cancellation checks run between final construction stages and before return.

## Range, schema and identity

Ranges are start-inclusive and end-exclusive. Calendar dates include the final
fractional second and end at the following point-local midnight. In a date
pair, the final date is included through its following midnight. `today` and
`yesterday` use the point-local date; `HistoryClock` makes the current instant
injectable. A zero-width explicit DateTime range is empty and complete; a
reversed range is invalid. DST days can contain 23 or 25 hours. Ambiguous or
nonexistent calendar midnights produce an explicit validation error. The scoped
client requires the same unique point-local midnight and matching offset for
calendar and relative-date replies; explicit DateTime bounds retain their
instant-based cross-zone semantics.

The point must declare `his`, `kind` and `tz`. This selected strict H4 profile
also requires a registered `unit` on every Number point, checked before provider
access and in shared wire/client schema validation. H4 supports Bool, Number and Str
schemas, plus NA error samples. Values retain their original Kind: Int/Float,
Null/None/Remove, Ref, nominal and other rich values are unsupported by this
selected profile. Nothing is coerced, silently dropped, or replaced with Null.
A Number sample can be unitless or use the same registered unit identity as the
point. Unit spelling and admitted numeric bits are preserved. The selected H4
profile admits only the canonical unitless `f64::NAN` NaN representation; other
NaN bit patterns and every unit-bearing NaN are unsupported. Canonical unitless
NaN, infinities, and finite Number bits are preserved. The pinned JSON dependency
uses its `float_roundtrip` parser feature to preserve finite IEEE-754 originals;
this changes no dependency version or core default feature. A present non-string JSON
v4 Number unit is a codec error.

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
evicted timestamp. `Complete` must include each known retained first/last endpoint lying inside the
requested half-open interval, and the retained count when both are enclosed.
These checks span all batches; a contradiction fails while preserving admitted
partial samples. `Complete` covers retained records in the requested interval;
it never claims to reconstruct data removed by retention. The native memory
store retains at most one million items per point by default. Its trusted
`write` and inclusive-end, materializing `read` compatibility methods are outside
the authorized bounded service. Missing-series observations now allocate one
bounded, stable generation-zero identity. Authorized scoped writes and retained
receipts use that same authority; see [history mutations](history-mutations.md).

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
selected history appears in scoped operation discovery. Scoped `hisWrite` and
`hisReceipt` require a separate explicit history mutation selection. External routers and owned listeners consume the exact
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

Scoped H4 history output checks every timestamp before projection: `hisStart`,
`hisEnd`, evaluated time, coverage boundaries and samples. Each must have a checked
local conversion into calendar years 0000 through 9999, a whole-minute UTC offset,
and leap nanoseconds only on a stored second field of 59. Representable leap
fractions keep their exact nanoseconds. Unsupported timestamps fail explicitly,
including in empty read responses whose headers contain historical second-level
offsets. Native typed collection and trusted stored timestamps retain their
original fields and values.

## Incremental Rust HTTP consumption

`HaystackClient<HttpTransport>::his_read_stream(request, options)` consumes the
same finite `bounded-history-v1` response incrementally over Zinc 3.0
(`text/zinc`). It uses the configured client's CA trust, mTLS identity, auth and
HTTP timeout, with redirects, automatic retries and implicit decompression
disabled. Other configured formats fail before dispatch. The new API changes
client consumption; the scoped server still constructs one bounded response.

```rust,no_run
use haystack_client::{HaystackClient, history::{HistoryStreamEnd, HistoryStreamOptions}};
use haystack_core::codecs::history::HistoryReadRequest;
# async fn read() -> Result<(), Box<dyn std::error::Error>> {
let client = HaystackClient::connect("https://example.com/api", "user", "password").await?;
let request = HistoryReadRequest { id: "point".into(), range: "2024-06-01".into() };
let mut session = client.his_read_stream(&request, HistoryStreamOptions::default()).await?;
// These are remote observations. The header's terminal is provisional.
let metadata = &session.header().metadata;
println!("history generation {}", metadata.history.generation);
loop {
    let batch = session.next().await;
    for sample in batch.samples {
        // Consume or persist each bounded batch without collecting the response.
        println!("{:?}", sample);
    }
    if let Some(end) = batch.end {
        match end {
            HistoryStreamEnd::Remote(terminal) => println!("validated remote terminal: {terminal:?}"),
            HistoryStreamEnd::Local(error) => eprintln!("partial read: {error}"),
        }
        break;
    }
}
session.close().await;
# Ok(())
# }
```

Every remote metadata field is retained: schema/unit/timezone, requested and
evaluated ranges, graph/catalog/policy observations, history identity, full
retained coverage, bounds, returned count and declared terminal. Local batch
boundaries do not replace those observations. `Remote(Complete)` is emitted only
after transport EOF, exact row count, strictly ordered instants, schema, range
and retained-endpoint/count validation. A count match alone is insufficient when
complete coverage requires a missing retained record. `Remote(Limited(...))`,
`Remote(Interrupted(...))` and `Remote(Failed(...))` preserve the server's outcome.
A local protocol, transport, limit, cancellation or deadline failure is a
separate `Local(...)` end alongside any admitted prefix; it never becomes remote
completion. Earlier batches remain partial until the final end is observed.

The selected grammar requires one Zinc 3.0 metadata line, exactly `ts,val`, and
newline-terminated scalar rows. CRLF is admitted; a single final blank line is
admitted. Duplicate/unknown metadata, alternate columns or column metadata,
comments, nested/rich values, raw carriage returns or line feeds in string cells,
surplus cells and trailing garbage are rejected. Escaped `\r` and `\n` keep their
exact values and remain distinct from literal backslash-plus-`r` or `n`.
Date/time scalar syntax is checked for ASCII before decoding, including
malformed values of the wrong kind, so Unicode cannot enter fixed-byte temporal
parsing. Non-ASCII or compound request-range syntax is rejected before dispatch.
Strings, Bool, NA and the admitted Number representation keep
their existing H4 semantics, including unit aliases, exact numeric bits,
timezone spelling, historical offsets and representable minute-leap fractions.

Each transport admits at most 16 opening/live sessions, rejecting excess calls
immediately. Each session owns one task, one response and one decoder. At most
one pull is outstanding. Abandoning a `next()` wait preserves its requested
batch; no background decode continues after that batch is ready. The owner
continues to observe the original absolute deadline and cancellation while the
consumer is idle. Explicit `close()` is idempotent and joins actual owned task,
response and parser cleanup. Abandoning a close wait preserves that join for a
later close. Drop signals cancellation and requests task abort without claiming
a synchronous join. Sessions own their lifetime independently of the parent
client; closing that client does not close retained sessions. The configured
HTTP request timeout remains an additional bound and is not renewed per batch.

Default local ceilings are 16 MiB response/source bytes, 10,000 rows, a 128 KiB
encoded metadata line, a 64 KiB encoded row, 32 KiB decoded field, 512 KiB copied
HTTP DATA frame, 64 rows/64 KiB conservative source bytes per batch, and 256 MiB
of work accounting. The work counter charges four times received encoded bytes
plus admitted source bytes; source accounting reserves 512 bytes per sample.
The typed control also retains the shared 16 KiB, depth-12, 512-node limits.
Options can tighten the total profile ceilings. Remote response and batch
bounds can further tighten the local selections. Limits are checked before
extending a line or decoding a field, and cumulative semantic validation retains
only the envelope, counters and first/last timestamps. Framing retains one
bounded copied frame, one bounded partial line and at most one decoded sample
that did not fit the preceding batch, in addition to the bounded ready batch.

`HistoryStreamStats` reports received/admitted counts and framing/batch high
watermarks; it is not an allocator measurement. Reqwest exposes a native DATA
frame only after receiving it, so an oversized native frame may already exist
before the client rejects it without copying. HTTP/TLS read-ahead, native-frame
allocation, allocator overhead and kernel buffers are outside the batch limits.
The qualified locked stack was reqwest 0.13.5, Hyper 1.12.0 and rustls 0.23.46;
observed Hyper HTTP/1.1 DATA frames reached 417,792 bytes. This is dependency
behavior, not a promised API frame size. No claim equates a batch with a TCP,
TLS or HTTP frame.

Native retained-memory qualification on macOS arm64 used separate client
processes with two runtime workers (active during the consumer pause) and a
fixture server in the parent process, 10,000 rows, identical
four-row batch limits, and 1,332,264-byte versus 14,332,264-byte responses. Across
three fresh-process pairs, post-warmup idle RSS increases were 128–240 KiB for
the stream. The deliberately collecting control retained 2,672–2,752 KiB for the
small response and 28,464–28,592 KiB for the large response. The predeclared
criteria were at most 4 MiB additional streaming RSS as the body grew and at
least 6 MiB separation in the collecting control. All pairs passed. Decoder
counters stayed unchanged while demand was paused; large-response server writes
stalled between 819,004 and 1,744,700 bytes and resumed after demand. This combines measured RSS,
observed socket backpressure and bounded-state inspection; it does not measure
kernel memory or qualify another OS/allocator/transport implementation.

Run that explicit native qualification with:

```sh
cargo nextest run --locked -p rusty-haystack-client \
  --test history_stream_memory --run-ignored ignored-only \
  -E 'test(retained_memory_scales)' --success-output immediate
```

It needs local loopback sockets, permission to launch and inspect its own child
processes, and a `ps -o rss= -p PID` implementation reporting KiB. It is ignored
by ordinary tests and is not wired into hosted CI. Functional tests run normally:
chunk splitting, malformed/partial responses, capacity recovery, cancellation,
idle deadlines, dropped waits, Basic/SCRAM, CA/hostname and mandatory mTLS.
The actual peer-retirement proof uses incomplete HTTP/1.1 bodies plus peer
EOF/reset and joined handlers. It establishes that selected fixture's network
cleanup; it does not establish HTTP/2 stream retirement. The API does not add a
local application provider adapter, Arrow/Parquet, Python/CLI streaming, a server
pull protocol, or an atomic remote snapshot guarantee.
