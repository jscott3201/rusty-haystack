# Parquet history captures

`rusty-haystack-analytics` (library `haystack_analytics`) exports one completed
remote `bounded-history-v1` read into bounded Arrow record batches and an
immutable local Parquet dataset, then reopens that dataset with validated,
bounded readback. The source is exactly one `HttpHistorySession` from
`HaystackClient::his_read_stream` (see the
[history client contract](history-reads.md#incremental-rust-http-consumption)).
One export covers one point and one requested range.

The crate is a separate workspace member. Core, client, server and application
default features are unchanged, and nothing else in the workspace depends on it.
It is a library profile only: there is no CLI command, Python binding, MCP tool,
server route or DataFusion integration. `arrow-array`, `arrow-schema` and
`parquet` are pinned exactly in the root `Cargo.toml`; `parquet` is built with
only its `arrow` feature, so no compression codec is compiled in. The pin
follows the Arrow family of the DataFusion release planned for a later slice;
this slice does not depend on DataFusion.

A dataset is published only when the remote read ends `Remote(Complete)` and
every sample has passed the same cumulative validator the client uses. A
`Limited`, `Interrupted` or `Failed` remote outcome, a local stream error,
trailing garbage, cancellation or an expired deadline never publishes a prefix.

## Export and reopen

```rust,no_run
use haystack_analytics::{Dataset, Export, ExportOptions};
use haystack_client::{HaystackClient, history::HistoryStreamOptions};
use haystack_core::codecs::history::HistoryReadRequest;
# async fn capture() -> Result<(), Box<dyn std::error::Error>> {
let client = HaystackClient::connect("https://example.com/api", "user", "password").await?;
let request = HistoryReadRequest { id: "point".into(), range: "2024-06-01".into() };
let session = client.his_read_stream(&request, HistoryStreamOptions::default()).await?;

// The destination must not exist yet. Pass an unconsumed session.
let mut export = Export::start(session, "/data/captures/point-2024-06-01", ExportOptions::default())?;
let receipt = export.wait().await?;
println!("{} rows in {} parts", receipt.manifest.rows, receipt.manifest.parts.len());

// Dataset is blocking (file I/O and decoding): keep it off async workers.
tokio::task::spawn_blocking(|| -> Result<(), haystack_analytics::Error> {
    let mut dataset = Dataset::open("/data/captures/point-2024-06-01")?;
    println!("dataset identity {}", dataset.manifest_sha256());
    assert!(!dataset.semantically_complete());
    while let Some(samples) = dataset.next_batch()? {
        for sample in samples {
            println!("{:?}", sample);
        }
    }
    // Only now has every row been checked against the recorded header.
    assert!(dataset.semantically_complete());
    Ok(())
})
.await??;
# Ok(())
# }
```

Public items, all re-exported from the crate root:

| Item | Role |
|---|---|
| `Export` | Owns one running export: `start`, `cancel`, `wait`, `close` |
| `ExportOptions` | `limits`, absolute `timeout` from admission, caller `cancellation` token |
| `ExportReceipt` | The published `Manifest` plus `ExportStats` (`#[non_exhaustive]`) |
| `ExportStats` | Source pulls, Arrow batch count, peak source/Arrow/writer batch sizes (`#[non_exhaustive]`) |
| `Dataset` | Lazy bounded reader of a committed dataset: `open`, `manifest`, `manifest_sha256`, `header`, `next_batch`, `semantically_complete` |
| `Limits` | Per-export ceilings, recorded in the manifest |
| `Manifest`, `Part` | The versioned JSON manifest and its part entries |
| `MANIFEST_NAME` | The published manifest file name |
| `MAX_CONCURRENT_EXPORTS`, `DEFAULT_EXPORT_TIMEOUT` | Admission capacity and the default `ExportOptions::timeout` |
| `Error` | Export and readback failures (`#[non_exhaustive]`) |

`Limits` and `ExportOptions` stay exhaustive so callers can keep writing
`Limits { batch_rows: 32, ..Default::default() }`.

`Export::start` returns `Error::Runtime` when called outside a Tokio runtime
(the session is dropped). It then validates `Limits`, computes the absolute
deadline and takes an admission permit before spawning the worker.
Everything that touches the filesystem — including rejecting an existing
destination — happens in the worker, so those failures are returned by `wait`,
not `start`. `ExportStats` byte figures are source/Arrow buffer accounting and the
Parquet writer's own estimate; they are not allocator or RSS measurements.

## Arrow schema and value mapping

Every part has the same flat nine-column Arrow schema, defined by
`format::schema` in `haystack-analytics/src/format.rs`:

| Column | Arrow type | Nullable | Contents |
|---|---|---|---|
| `point_id` | Utf8 | no | The header's point id on every row |
| `utc_seconds` | Int64 | no | Whole seconds since the Unix epoch, UTC |
| `nanoseconds` | UInt32 | no | Sub-second nanoseconds as chrono stores them |
| `offset_seconds` | Int32 | no | The sample's own UTC offset |
| `present` | Boolean | no | `false` exactly for an NA sample |
| `bool_value` | Boolean | yes | Bool samples only |
| `number_value` | Float64 | yes | Number samples only |
| `string_value` | Utf8 | yes | Str samples only |
| `sample_unit` | Utf8 | yes | Number samples with a unit only |

Schema metadata records `haystack.schema` (`1`), `haystack.kind`,
`haystack.timezone` and `haystack.point_unit` (empty when the point has none).
Readback derives the expected schema from the manifest's recorded header and
compares fields; it ignores the Arrow metadata embedded in the Parquet footer.

The admitted values are exactly those of the selected H4 history profile,
checked with the shared `valid_value` for the point's declared kind:

- **Bool** points: `bool_value` set, every other value slot and `sample_unit` null.
- **Number** points: `number_value` holds the original IEEE-754 binary64 bits —
  `-0.0`, both infinities, the canonical unitless NaN and adjacent finite values
  are tested bit-for-bit through an independent Parquet reader. `sample_unit` is
  the sample's own unit spelling, so a unit alias such as `fahrenheit` on a `°F`
  point is retained as written; a unitless sample has a null unit. The unit
  identity and NaN rules are the history profile's: a unit-bearing NaN or a
  non-canonical NaN payload is rejected.
- **Str** points: `string_value` holds the exact UTF-8 string, including control
  characters, quotes and an empty string.
- **NA** (any kind): `present = false` and all four value columns null. NA is
  therefore distinct from a NaN Number, which has `present = true` and a
  non-null `number_value`. A null value slot means only "not this sample's
  kind" or NA; readback rejects any other combination of slots.

Timestamps are stored as components, not an Arrow timestamp type. The instant is
`utc_seconds` plus `nanoseconds`; `offset_seconds` restores the sample's actual
historical offset (both sides of a DST fold are tested) and the point timezone
name comes from the header. `nanoseconds` can exceed 999,999,999 to carry a
representable leap-second fraction, which round-trips exactly.

Anything else fails the export: values of the wrong kind for the point, Marker,
Ref, List, Null, None, Remove, `Int`, `Float` and other rich values are rejected,
not coerced or dropped. A string or unit longer than `field_bytes` is
`Error::Limit`.

## Provenance and the manifest

The manifest is JSON, `deny_unknown_fields`, with `manifest_version` and
`schema_version` both `1`, a random v4 UUID `export_id`, the consistency label
`completedGenerationCheckedLiveCapture`, the `Limits` the export ran under, the
total row count and an ordered list of `Part` entries (ordinal, file name,
length, SHA-256, rows, row groups).

`source_header` is the remote header re-encoded as the existing two-line Zinc
`bounded-history-v1` result envelope (via the core `result_header_grid`), with no
rows. It retains every remote metadata field the client validated: point id,
schema kind/unit/timezone, requested and evaluated range, graph, catalog and
policy observations, history authority/incarnation/generation, capabilities,
retained coverage, returned count and the `Complete` terminal. Counters larger
than 2^53 are carried as decimal strings and survive reopen exactly. No URL,
credential or remote error text is stored, and `Error` diagnostics never include
them.

## Bounds

All ceilings are finite. **The constants are the documentation**; their values
are deliberately not restated here.

| Bound | Where |
|---|---|
| Per-export `Limits` (rows and conservative bytes per Arrow batch, decoded field bytes, row groups per part, bytes per part and in total, part count, manifest and footer bytes) | `Limits::default()` and `MAX_GROUPS_PER_PART` in `haystack-analytics/src/format.rs`; `Limits::validate` rejects any option above those defaults or below `MIN_BATCH_BYTES`, `MIN_PART_BYTES`, `MIN_MANIFEST_BYTES`, `MIN_FOOTER_BYTES` |
| A part must fit the writer reserve of one full `batch_bytes` batch (checked by `validate` before any network read; necessary, not sufficient) | `writer_reserve` in `format.rs` |
| Concurrent exports per process, with no wait queue | `MAX_CONCURRENT_EXPORTS` in `haystack-analytics/src/export.rs` |
| Default absolute export timeout | `DEFAULT_EXPORT_TIMEOUT` in `export.rs` |
| Largest admitted source batch (an overlap check on top of the client's own limits) | the shared `bounded-history-v1` ceilings `MAX_GRID_BYTES` and `MAX_ROWS` in `haystack-core/src/codecs/history.rs`, applied in `run` in `export.rs` |
| Writer admission reserve per batch, and the encoded ceiling of a row group or page | `writer_reserve` and `encoded_ceiling` (`ENCODING_EXPANSION`, `ENCODING_SLACK_BYTES`) in `format.rs` |
| Parquet page size target and writer step | `DATA_PAGE_BYTES`, `WRITE_BATCH_ROWS` in `haystack-analytics/src/writer.rs` |
| Byte-capped part sink that also caps footer bytes | `CappedFile` in `writer.rs` |
| Manifest read size | `Limits::default().manifest_bytes`, read by `Dataset::open` in `haystack-analytics/src/reader.rs` |
| Total rows on reopen | `MAX_ROWS` in `haystack-core/src/codecs/history.rs` |
| Recorded header size and shape | `MAX_HEADER_BYTES`, `HEADER_LINES` and `header_preflight` in `format.rs` |
| Footer and page-header Compact Thrift preflight (depth, node count, field ids, list/map sizes, schema width, row groups) | `COMPACT_MAX_DEPTH`, `COMPACT_MAX_NODES`, `COMPACT_MAX_FIELD_ID`, `COMPACT_MAX_LIST`, `COMPACT_MAX_MAP`, `FOOTER_MAX_SCHEMA_ELEMENTS`, `MAX_PAGE_HEADER_BYTES` in `reader.rs` |
| Column-chunk and page sizes, pages per chunk, level and payload framing | `checked_metadata`, `page_guard`, `definition_levels` in `reader.rs` |

`ExportOptions::limits` can only tighten the defaults. Source batches are split
into Arrow batches by `batch_rows` and by the shared conservative sample charge
(`HistoryReadValidator::sample_bytes`, 512 bytes per sample plus its variable
string or unit bytes); a single sample that does not fit is `Error::Limit`. Each
Arrow batch is one Parquet row group. A part is closed when it reaches
`groups_per_part` or when the next batch's reserve would exceed `part_bytes`.
Writer properties are fixed: uncompressed, PLAIN values with RLE levels, no
dictionary, statistics, bloom filters, column index or offset index.

One export holds at most one source batch, one Arrow batch and one open column
writer at a time. The writer runs on Tokio's blocking pool, and no further
source batch is pulled while a write is in progress.

## Publication protocol

1. Create the destination directory with `create_dir` (it must not exist; an
   existing directory is left untouched and the export ends with
   `Error::Io(AlreadyExists)`), then `fsync` its parent. On Unix the directory
   is created `0o700` and every part and manifest file `0o600` (owner only,
   as the CLI's backup output), subject to the process umask only narrowing them.
2. Write parts `part-0000.parquet`, `part-0001.parquet`, … with `create_new`.
   Each finished part is footer-written and `fsync`ed, then read back once:
   the reader's own footer, layout and page guards run over those exact bytes
   and the same bytes are hashed with SHA-256. An export therefore never
   publishes a part its reader would reject; a footer over `footer_bytes`
   surfaces here as `Error::Limit`.
3. After the source reports `Remote(Complete)`, cumulative validation finishes
   and the source session is closed, finish the last part and `fsync` the
   directory.
4. Write the manifest to `.manifest.pending` with `create_new` and `fsync` it.
5. Check cancellation and the deadline once more, then publish by hard-linking
   `.manifest.pending` to `manifest.json`. A hard link never replaces an existing
   name, so a pre-existing `manifest.json` fails with `Error::Io(AlreadyExists)`
   and is not modified.
6. `fsync` the directory, remove `.manifest.pending`, `fsync` the directory again.

The link in step 5 is the commit point. Any failure before it — I/O, Parquet,
limit, validation, cancellation, deadline — returns that error and leaves
**no** `manifest.json`; the incomplete directory is left in place for inspection
and is not cleaned up. Any failure after it returns
`Error::PublishedUncertain { export_id }`: the manifest name exists and the
dataset reopens and reads completely, but the durability acknowledgment for the
directory entry was not obtained. Cancellation or deadline expiry after the link
cannot revoke publication, and the export returns success.

## Reopen and validation

`Dataset::open` reads only a committed `manifest.json` (a regular file, not a
symlink, opened once with type and length checked on the handle; larger than
the hard `manifest_bytes` ceiling is `Error::Limit`). `manifest_sha256()` is
the SHA-256 of exactly those bytes, a stable identity for the dataset since the
manifest pins every part hash. `open` verifies before returning:

- manifest size, versions, consistency label, UUID, `deny_unknown_fields`, and
  recorded `Limits` that themselves pass `Limits::validate`;
- the recorded header decodes, is self-consistent for its own request, ends
  `Complete`, and its returned count equals the manifest's row total;
- each part's ordinal and exact generated name (no path from the manifest is
  trusted), regular-file length, SHA-256, row and group counts;
- each footer through a bounded Compact Thrift preflight before the upstream
  parser allocates (a repeated or out-of-range field id is rejected, since a
  preflight and the upstream decoder could resolve it differently), then the
  physical schema, row-group count and row counts;
- every column chunk is contiguous, inside the file, ends exactly at the
  footer, uncompressed, PLAIN/RLE only, of the expected physical type and
  level structure, without dictionary, statistics, bloom filter, page index or
  external file path. Offsets and sizes are checked as signed values first, so
  upstream helpers that assert on them never see a negative one;
- every page header is bounded and accounts for exactly the group's rows, and
  every page body is framed before decode: an optional column's 4-byte level
  length must stay inside the page and its bit-width-one RLE/bit-packed runs
  must parse, consume exactly that length and cover exactly the page's value
  count; the PLAIN payload must be exactly the size its physical type implies
  for the non-null count (length-prefixed byte arrays must tile it exactly).

The recorded header passes a lexical preflight before the general Zinc
parser: outside its quoted string only printable ASCII and the line separator,
no containers; inside it, no raw control characters.

These guards are the contract. As a backstop, upstream decoder calls also run
inside `catch_unwind`, so a panic the guards did not anticipate becomes
`Error::Invalid` (and poisons the reader) rather than unwinding into the
caller. The process panic hook still runs. The backstop is not relied on: the
mutation test below disables it.

Opening verifies integrity and layout, not semantics. `next_batch` decodes one
row group at a time and checks every row: point id, non-null required columns,
exactly the value slots allowed for the kind, units only on Numbers, valid
timestamp components and field sizes. Each sample also passes the cumulative
history validator: range, strict ordering, timezone and coverage. Only when
`next_batch` returns `Ok(None)` after consuming every part does
`semantically_complete()` become `true`; a hash-valid dataset whose rows
contradict its header (for example a coverage count that only a missing row
could satisfy) fails at the end and never reports completion. After any error
the reader is poisoned and keeps returning `Error::Invalid`.

`next_batch` holds one part in memory at a time, at most the recorded
`part_bytes`. It opens each part once, hashes those exact bytes against the
manifest again, reruns the footer and layout guards on them and decodes from
the same buffer, so a part replaced after `open` is `Error::Invalid`, never
decoded unchecked. `open` keeps its own up-front pass so corruption is
rejected before any row is returned.

`Dataset::open` and `next_batch` are blocking (file I/O and decoding); call
them from `tokio::task::spawn_blocking` in async code.

## Cancellation, deadlines and drop

- `ExportOptions::timeout` is absolute from admission and includes idle network
  time. `ExportOptions::cancellation` is a parent token; `Export::cancel` cancels
  only this export's child token.
- Both are cooperative and checked between steps, while waiting for a source
  pull, and immediately before the commit link. A filesystem job that has
  started is always joined first, so `wait` reports `Deadline` or `Cancelled`
  only after that job has actually finished.
- `wait` and `close` are cancellation-safe. Dropping either future leaves the
  worker handle in `Export`, and a later call observes the same result; the
  result is retained and cloned for repeated calls.
- `close` requests cancellation, then joins the source session and any started
  writer. Because publication may already have committed, it can return success
  or `PublishedUncertain`.
- Dropping `Export` requests cancellation but does not join. The worker still
  finishes its current filesystem job and closes the source; the admission
  permit is released only when the worker ends.
- On every path that does not publish, the worker waits for the source session
  to close before returning.

## Evidence

Ordinary tests (`cargo test -p rusty-haystack-analytics`) run against a loopback
HTTP fixture: multi-part captures with exact provenance, empty and terminal-batch
captures, value bits/NA/units/strings and timestamp components checked through
an independent Parquet reader, non-`Complete` and trailing-garbage sources,
injected failures and cancellations at every write/sync/publish boundary,
deadline expiry during a held write, fresh-destination and no-clobber
publication, admission without queueing, drop while a write is held, manifest
and part corruption, hash-valid semantic corruption, Compact Thrift allocation
and recursion bombs, page level and PLAIN payload framing, byte-driven batch
and part splitting that round-trips, `total_bytes`/`parts`/`manifest_bytes`/
`footer_bytes` overruns that publish nothing, oversize manifests and row
totals, a part swapped for a hash-different, same-length, structurally valid
one after `open` (rejected at read), owner-only modes on Unix, and — on Unix
only — a child process killed with SIGKILL just before and just after the
commit link (before: no dataset; after: reopens with every sample).

`hostile_byte_mutations_are_rejected_by_guards_without_panicking` disables
the `catch_unwind` backstop on its thread and asserts that no panic escapes:
every byte of a mixed-level part set to `0xff` plus one seeded classic
mutation per byte (hash recomputed, read through `Dataset`), every header
character replaced by non-ASCII and control characters, a seeded sample of
grammar substitutions, and the original non-ASCII header repro through
`Dataset::open`. With any one of the negative-offset, level or header guards
reverted, it fails.

Retained memory was observed once on a local macOS release build, with the
export held just before publication and process RSS sampled from a separate
child per measurement. Each sample was a 1,000-byte string; limits were 32-row,
64 KiB batches, four row groups per part and 512 KiB parts:

| Mode | 500 rows | 9,000 rows (~9.3 MB wire, 71 parts) |
|---|---|---|
| Streaming export, retained RSS increase (three repeats) | 1,360–1,456 KiB | 2,432–2,864 KiB |
| Collect-everything control | 1,344–1,392 KiB | 13,136–13,648 KiB |

Peak batch sizes in the large export were 48,384 bytes (source), 36,452 bytes
(Arrow) and 9,768 bytes (Parquet writer estimate). The predeclared criteria were
at most 4 MiB of additional export RSS from 500 to 9,000 rows and at least
6 MiB of growth in the collecting control. This is an observation of one
machine, allocator and runtime, **not a supported memory bound**; it does not
measure kernel page cache or qualify another platform. It measures the export
only. The writer's read-back verification holds one finished part at a time;
the reader likewise holds one part's bytes (at most `part_bytes`) plus one
decoded batch, and its memory is not measured here. Reproduce it with:

```sh
cargo test -p rusty-haystack-analytics --release --lib memory_scales -- --ignored --nocapture
```

It needs loopback sockets, permission to launch and inspect child processes and
a `ps -o rss= -p PID` that reports KiB. It is ignored by ordinary tests and is
not part of hosted CI.

## Not claimed

- **No atomic source snapshot.** The selected providers advertise
  `snapshots = false`. A capture is one generation-checked live read that
  completed; it is not a point-in-time snapshot of the entity graph and history
  together. The consistency label says exactly that.
- **No power-loss qualification.** The `fsync` sequence is the intended
  durability protocol, but the crash evidence is a killed process with the
  kernel intact. Power loss, network or FUSE filesystems, and filesystems
  without hard links or directory `fsync` are not qualified.
- **No Windows durability qualification.** Directory `fsync` opens the
  directory as a file, which is a Unix behaviour; on other platforms the
  directory sync steps are skipped (NTFS journals metadata, but its link and
  rename durability semantics are not the Unix ones and are not tested). The
  no-replace publish still holds there: `CreateHardLinkW` refuses an existing
  name. Windows CI runs the ordinary tests; the process-kill and permission
  tests are Unix-only.
- **No query engine or other front ends.** No DataFusion, Python, CLI, MCP or
  server exposure.
- **No overwrite, append, merge, compaction or resume.** Each export writes a
  new directory once. A failed export is not resumed or cleaned up.
- **No object stores or remote destinations.** Local paths only.
- **Single point, single range, single remote source.** No multi-point
  datasets, local application provider adapter or other wire formats.
- **No general Parquet reader.** `Dataset` reads only datasets in this exact
  layout. Other Parquet tools can read the parts, but the manifest, not the
  directory listing, defines the dataset.
