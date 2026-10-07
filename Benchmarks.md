# Rusty Haystack Benchmarks

## Reproducible baseline

The `baseline` targets provide a bounded, correctness-checked selection for future
comparisons. They make no production workload, memory-footprint, cross-platform,
concurrency-capacity, or speedup claim. Timings are collected only after the harness
is committed and the measurement host is free of other builds and benchmarks.

| Dataset | Exact shape | Queries / expected rows |
|---|---|---|
| `campus_v1`, 972 entities | 12 campuses × (1 site + 8 equips + 72 points) | Marker: 96; site reference: 16; nested reference: 90; after referenced-site mutation: 75 |
| `campus_v1`, 10,125 entities | 125 campuses × the same 81-entity shape | Marker: 1,000; site reference: 16; nested reference: 945; after mutation: 930 |
| `history_v1`, 1,000 items | One series, UTC minute steps from 2024-06-01; value `70 + index/4` | Full and first-day reads: 1,000 |
| `history_v1`, 10,000 items | Same series specification, spanning almost seven days | Full: 10,000; first UTC day: 1,440 |

The campus generator alternates Portland/Seattle sites and uses AHU, VAV, boiler,
meter, and weather equipment, with mixed sensor/command/setpoint tags. Query IDs and
expressions are recorded by the harness. Independent manually enumerated IDs are
checked against both the index-free evaluator and each indexed/cache path. The
mutation moves site-0 from Portland to Seattle after warming the nested-reference
result, requiring exactly 15 point IDs to disappear. History validation checks every
timestamp and value, not only cardinality, and detects the old repeating-day bug.
Validation uses separate graph/store/server instances that are discarded before
timed instances are prepared.

Every core sample times `EntityGraph::read(expression, 0)`, including returned-grid
construction. Each iteration receives a fresh graph in untimed setup:

| State | Untimed preparation | Timed work |
|---|---|---|
| `cold` | Build graph; no reads | Parse, evaluate, populate result cache, build grid |
| `ast_warm_result_cold` | Read with `usize::MAX`, which caches the AST but not results | Reuse AST, evaluate, populate result cache, build grid |
| `result_warm` | Read the same query with limit 0 | Reuse cached identities and build grid |
| `mutation_then_nested_query` | Warm the nested query with limit 0 | Construct/apply site patch, reevaluate invalidated result, build grid |

`iter_batched_ref` with `BatchSize::PerIteration` excludes setup and destruction of
the input graph and output grid. It retains one prepared graph per iteration, so
the harness does not grow an insertion workload or accumulate batches of graphs.
These dataset caps are not measured RSS or a production memory budget. Hashes use
sorted entity IDs and tags with Zinc scalar values; history hashes use ordered
RFC3339 timestamps and values. Hashes and fixture metadata are saved with each run.

Direct history timings include the store lock, scan and row cloning. HTTP timings
include one synchronous `block_on`, request encoding, a loopback HTTP round-trip,
server handling, and response decoding. Both exclude returned-container destruction.
The HTTP server and client are created outside the timing loop, and the client
reuses connections after warmup. Authentication is intentionally disabled. The
fixture binds port 0, receives the actual address, verifies an HTTP readiness
response, owns a two-worker Tokio runtime, and cancels/joins the server at teardown.
There is one caller and one outstanding request; these are not saturation tests.

The harness defaults to 30 Criterion samples, 500 ms warmup, a 2-second measurement
target, and 10,000 bootstrap resamples. Criterion confidence intervals describe its
sample estimator, not p95/p99 request latency. Actual sampling modes, iteration
counts, raw times, toolchain, lock digest, source revision/dirty state, build profile,
features, host, workers and exact commands are retained by the capture script.

```bash
# Validate the benchmark data and real HTTP fixture; no performance claim.
cargo +1.99.0 nextest run --locked -p rusty-haystack-core -p rusty-haystack-server --test benchmark_fixtures --no-tests=fail

# Smoke-test each Criterion target without collecting timings.
cargo +1.99.0 bench --locked -p rusty-haystack-core --bench baseline -- --test
cargo +1.99.0 bench --locked -p rusty-haystack-server --bench baseline -- --test

# Test capture input admission and receipts without builds or measurements.
python3 -m unittest discover -s scripts/bench -p test_capture.py

# From clean committed source, with other heavy work paused. Python 3.11+;
# destination must be outside the checkout and absent or empty.
python3 scripts/bench/capture.py /tmp/rusty-haystack-baseline
```

The capture script uses the worktree's own `target` cache, `--locked`, Cargo's bench
profile plus any recorded workspace manifest profile settings, `RUSTFLAGS=-Dwarnings`,
two build jobs, and no extra package feature flags. It resolves Cargo, rustc and
rustdoc through `rustup which --toolchain 1.99.0`, invokes those exact paths, and
records their hashes, Cargo/compiler versions and the compiler host target. No
`--target` is used.

Capture rejects inherited compiler/wrapper, Cargo target/profile/build selectors,
encoded Rust flags and native compiler/linker overrides. It controls `RUSTFLAGS`,
`CARGO_BUILD_JOBS`, `CARGO_INCREMENTAL` and `CARGO_TARGET_DIR` only in child processes;
the caller environment is unchanged. `CARGO_HOME` may locate an existing cache, but
any `config` or `config.toml` in that Cargo home, the workspace or its ancestors is
rejected before running. Configuration existence is checked without reading its
contents or credential files; diagnostics disclose override names, never values.
These checks follow Cargo's [configuration discovery and precedence rules](https://doc.rust-lang.org/cargo/reference/config.html).
The receipt records manifest profiles and absence of active Cargo configuration,
and completion rechecks inputs, toolchain identity and source. Use an invocation
environment and checkout with no such overrides; the script does not edit personal
configuration. This bounds Cargo build inputs without claiming a hermetic host.

Raw Criterion reports/logs remain in the specified output
directory; `baseline.json` retains the compact raw samples and provenance. Results
must cite their measured source commit even when the report is added in a later
commit. A smoke test or fixture test is not a timing result.

## Measured baseline — October 7, 2026

This run measured source commit
[`1a9edf75cf9fa0ce2cc7bb88ffc5e357ca1c131c`](https://github.com/jscott3201/rusty-haystack/commit/1a9edf75cf9fa0ce2cc7bb88ffc5e357ca1c131c).
The source was clean before and after capture, with an unchanged lock digest. This
identity refers to the measured harness; the later report commit is not substituted
for it. All 26 workloads completed successfully: 18 graph query/cache cases, two
mutation/query cases, and six history cases.

| Property | Observed value |
|---|---|
| Host | Apple M5, 10 logical CPUs, 16 GiB RAM; macOS 27.2, arm64 |
| Compiler / target | Rust 1.99.0 (`b940084d7`), LLVM 23.1.1; `aarch64-apple-darwin` |
| Cargo / Criterion | Cargo 1.99.0 (`5f94df478`); Criterion 0.8.2 |
| Build | Default bench profile; no manifest profile overrides or active Cargo config; package defaults; `RUSTFLAGS=-Dwarnings`; incremental off |
| Capture window | 2026-10-07 19:55:41.756304–19:57:04.126367 UTC; **82.37 seconds** total |
| Samples | 30 per workload; 780 retained samples; 500 ms warmup and 2-second measurement target per workload |
| Sampling mode | Linear for the 972-entity and history cases; flat for the 10,125-entity graph cases |

The worktree's release cache was prepared by prior optimized smoke tests. The two
Cargo invocations reported 0.11 and 0.10 seconds to finish preparation; the capture
duration also includes fixture checks/setup, warmups, sampling and analysis. A
coordinated quiet window paused other task builds, tests and benchmarks, with a
pre-run process check. This remained a shared interactive host: CPU placement,
thermal/frequency behavior and other background activity were not controlled.
There was one capture, without a repeated-run drift study.

Every value below is the Criterion **mean in microseconds**, followed by its **95%
bootstrap confidence interval**. These are mean estimates over Criterion samples,
not p95/p99 request latency. The raw receipt retains each sample's iteration count
and elapsed time as well as the full estimator output. The console's `time:` line
may use a different estimator, so the tables consistently select `estimates.mean`.

| Query | Entities | Cold | AST warm / result cold | Result warm |
|---|---:|---:|---:|---:|
| Marker | 972 | 85.29 [84.70, 86.02] | 79.33 [78.69, 80.04] | 44.47 [44.12, 44.84] |
| Site reference | 972 | 62.09 [61.76, 62.40] | 54.03 [53.82, 54.25] | 7.44 [7.39, 7.49] |
| Nested reference | 972 | 147.50 [146.92, 148.14] | 137.09 [136.78, 137.40] | 40.13 [39.96, 40.32] |
| Marker | 10,125 | 1,070.37 [1,055.47, 1,086.24] | 832.76 [828.05, 838.01] | 496.94 [494.74, 499.20] |
| Site reference | 10,125 | 728.50 [711.63, 746.18] | 457.09 [453.94, 460.62] | 8.95 [8.86, 9.05] |
| Nested reference | 10,125 | 2,049.00 [2,006.02, 2,098.43] | 1,601.45 [1,589.46, 1,615.53] | 462.86 [459.75, 467.07] |

| Mutation followed by nested query | Expected returned rows | Mean [95% CI], µs |
|---|---:|---:|
| 972 entities | 75 | 133.14 [132.57, 133.72] |
| 10,125 entities | 930 | 1,604.06 [1,587.55, 1,622.86] |

| History boundary | Stored items | Returned items | Mean [95% CI], µs |
|---|---:|---:|---:|
| Direct full series | 1,000 | 1,000 | 6.76 [6.73, 6.79] |
| Direct first UTC day | 1,000 | 1,000 | 7.14 [6.87, 7.66] |
| Loopback HTTP first UTC day | 1,000 | 1,000 | 1,213.68 [1,210.08, 1,217.82] |
| Direct full series | 10,000 | 10,000 | 66.51 [66.37, 66.65] |
| Direct first UTC day | 10,000 | 1,440 | 15.48 [15.46, 15.50] |
| Loopback HTTP first UTC day | 10,000 | 1,440 | 1,709.31 [1,705.14, 1,713.78] |

The exact [receipt and raw samples](scripts/bench/results/2026-10-07-1a9edf75/baseline.json),
[core log](scripts/bench/results/2026-10-07-1a9edf75/rusty-haystack-core.log) and
[history log](scripts/bench/results/2026-10-07-1a9edf75/rusty-haystack-server.log)
are retained together. The receipt includes dataset hashes, exact query expressions
and expected counts, commands, toolchain binary hashes, the lock digest, worker
counts, and log hashes. All samples and reported outliers are retained. Complete
Criterion output also remains in the original local capture directory recorded by
`environment.CRITERION_HOME`; the committed receipt contains the data needed to
inspect all 26 estimates without that directory.

These figures characterize only the stated synthetic fixtures and timing
boundaries on this host. They do not measure RSS, production workload behavior,
concurrent service capacity, cross-platform performance, or a before/after speedup.

## Historical report — March 2026

The tables below preserve the earlier v0.8.0 / Rust 1.93.1 report. They were not
remeasured for the current source, and their raw samples, precise source identity,
and cache states are not established by this document. They are not comparable to
the new baseline and do not substantiate current speedup or service-capacity claims.

## Environment

| Property | Value |
|----------|-------|
| Platform | macOS (Darwin 25.4.0, arm64) |
| CPU | Apple M2 |
| Memory | 8 GB |
| Rust | 1.93.1 |
| Version | 0.8.0 |
| Profile | release (optimized) |
| Framework | Criterion 0.8 |
| Date | 2026-03-24 |

---

## Core Benchmarks (haystack-core)

74 benchmarks covering codecs, filtering, graph operations, ontology, type checking, auth, units, traversal, and validation.

### Codec — Zinc

| Benchmark | Mean | Ops/sec |
|-----------|------|---------|
| `zinc_encode_scalar` | 53.0 ns | 18,868,000 |
| `zinc_decode_scalar` | 102.8 ns | 9,728,000 |
| `zinc_encode_100_rows` | 40.9 µs | 24,450 |
| `zinc_decode_100_rows` | 72.9 µs | 13,717 |
| `zinc_encode_1000_rows` | 580.4 µs | 1,723 |
| `zinc_decode_1000_rows` | 924.8 µs | 1,081 |

**Observations:**
- Zinc scalar encode at 53ns = ~18.9M ops/sec
- 100-row Zinc encode at 40.9µs = ~2.44M rows/sec throughput
- Zinc is 2.1x faster than JSON v4 for encoding and 1.7x faster for decoding at 100 rows

### Codec — JSON v4

| Benchmark | Mean | Ops/sec |
|-----------|------|---------|
| `json4_encode_100_rows` | 87.8 µs | 11,390 |
| `json4_decode_100_rows` | 123.8 µs | 8,077 |
| `json4_encode_1000_rows` | 947.5 µs | 1,055 |
| `json4_decode_1000_rows` | 1.327 ms | 754 |

**Observations:**
- JSON v4 at 87.8µs/100 rows encode — heavier than Zinc due to type wrappers (`{_kind, val}`)
- Scaling is roughly linear: 1000-row encode is ~10.8x the 100-row time

### Codec — JSON v3

| Benchmark | Mean | Ops/sec |
|-----------|------|---------|
| `json3_encode_100_rows` | 53.7 µs | 18,622 |
| `json3_decode_100_rows` | 73.6 µs | 13,587 |
| `json3_encode_1000_rows` | 562.2 µs | 1,779 |
| `json3_decode_1000_rows` | 744.4 µs | 1,343 |

**Observations:**
- JSON v3 is significantly faster than JSON v4 (~1.6x encode, ~1.7x decode at 100 rows) due to simpler type encoding
- JSON v3 encode performance is on par with Zinc at 100 rows (53.7µs vs 40.9µs)

### Codec — Trio

| Benchmark | Mean | Ops/sec |
|-----------|------|---------|
| `trio_encode_100_rows` | 61.5 µs | 16,260 |
| `trio_decode_100_rows` | 89.2 µs | 11,211 |

**Observations:**
- Trio sits between Zinc and JSON v4 in performance
- Tag-per-line format keeps encoding simple but slightly slower than Zinc's columnar layout

### Codec — CSV

| Benchmark | Mean | Ops/sec |
|-----------|------|---------|
| `csv_encode_1000_rows` | 613.4 µs | 1,630 |

**Observations:**
- CSV encode-only at 613.4µs/1000 rows — sits between Zinc (580.4µs) and JSON v3 (562.2µs)

### Codec — Roundtrip

| Benchmark | Mean | Ops/sec |
|-----------|------|---------|
| `codec_roundtrip_mixed_types` | 291.0 µs | 3,436 |

**Observations:**
- Full Zinc encode+decode roundtrip for 100 rows with 9 mixed types in 291µs
- Mixed types include Number, Str, Bool, Date, Time, DateTime, Uri, Ref, Marker

### Filter Engine

| Benchmark | Mean | Ops/sec |
|-----------|------|---------|
| `filter_parse_simple` | 57.5 ns | 17,391,000 |
| `filter_parse_complex` | 358.0 ns | 2,793,000 |
| `filter_eval_simple` | 7.8 ns | 128,205,000 |
| `filter_eval_complex` | 34.9 ns | 28,653,000 |

**Observations:**
- Simple filter evaluation at ~7.8ns = ~128M ops/sec — marker presence check
- Complex 4-clause evaluation at ~34.9ns = ~28.7M ops/sec
- Parse + eval combined for a simple filter is under 66ns
- AST caching eliminates re-parsing overhead for repeated queries

### Graph — Entity Operations

| Benchmark | Mean | Ops/sec |
|-----------|------|---------|
| `graph_get_entity` | 7.1 ns | 140,845,000 |
| `graph_add_entity` | 621.0 ns | 1,610,000 |
| `graph_add_1000_entities` | 1.483 ms | 674 |
| `graph_update_entity` | 1.264 µs | 791,139 |
| `graph_remove_entity` | 1.643 µs | 608,643 |
| `graph_changes_since` | 1.3 ns | 769,231,000 |

**Observations:**
- Entity lookup at ~7.1ns = ~141M ops/sec — single indexed get
- `graph_add_entity` at 621ns = ~1.61M adds/sec (with changelog and indexing)
- `changes_since` at ~1.3ns uses binary search on VecDeque — ~769M ops/sec
- Freelist ID recycling keeps entity IDs compact after remove+re-add cycles

### Graph — Filter Queries

| Benchmark | Mean | Ops/sec |
|-----------|------|---------|
| `graph_filter_1000_entities` | 418.8 µs | 2,388 |
| `graph_filter_10000_entities` | 4.622 ms | 216 |
| `graph_filter_realistic_10000` | 611.6 µs | 1,635 |
| `graph_filter_compound_10000` | 54.1 ns | 18,484,000 |
| `graph_filter_range_10000` | 1.227 ms | 815 |

**Observations:**
- The reported 54.1ns compound-filter figure has no established cache-state or raw
  sample provenance here; it does not establish bitmap evaluation cost or throughput.
- Realistic 10K dataset (diverse entity types) filters in 611.6µs vs 4.6ms for homogeneous — bitmap pruning is highly effective with diverse tag sets
- Range filter (`curVal > 73°F`) at 1.227ms scans all candidates with value comparison

### Graph — Scale & Optimization

| Benchmark | Mean | Ops/sec |
|-----------|------|---------|
| `graph_update_delta_10000` | 1.652 µs | 605,327 |
| `graph_freelist_recycle_1000` | 914.7 µs | 1,093 |

**Observations:**
- Delta update holds steady at ~1.65µs regardless of graph size — only re-indexes changed tags
- Freelist recycling of 1000 entities (remove + re-add) in 914.7µs = ~1.09M entity recycles/sec

### Dict & Grid Operations

| Benchmark | Mean | Ops/sec |
|-----------|------|---------|
| `dict_create_7_tags` | 327.9 ns | 3,050,000 |
| `dict_get_tag` | 6.8 ns | 147,059,000 |
| `dict_has_tag` | 6.6 ns | 151,515,000 |
| `dict_sorted_tags` | 52.9 ns | 18,904,000 |
| `dict_merge` | 127.1 ns | 7,868,000 |
| `graph_to_grid` | 44.7 µs | 22,371 |
| `graph_to_grid_filtered` | 41.3 µs | 24,213 |
| `graph_from_grid_112` | 142.2 µs | 7,032 |

**Observations:**
- Tag lookup (`get`/`has`) at ~6.6-6.8ns = ~148-152M ops/sec
- Dict creation with 7 tags at 328ns = ~3.05M dicts/sec
- Grid conversion for 112-entity graph in ~44.7µs; filtered variant slightly faster due to fewer entities

### SharedGraph (thread-safe)

| Benchmark | Mean | Ops/sec |
|-----------|------|---------|
| `shared_graph_get` | 172.3 ns | 5,804,000 |
| `shared_graph_read_filter` | 41.4 µs | 24,155 |
| `shared_graph_len` | 2.8 ns | 357,143,000 |
| `shared_graph_changes_since` | 27.1 µs | 36,900 |
| `shared_graph_concurrent_rw` | 221.2 µs | 4,521 |

**Observations:**
- SharedGraph `get` at 172.3ns includes RwLock acquisition overhead (~165ns over raw graph_get)
- `len` at 2.8ns is nearly free — atomic read
- Concurrent read/write (4 readers + 1 writer, 100 entities) at 221.2µs

### Ontology

| Benchmark | Mean | Ops/sec |
|-----------|------|---------|
| `ontology_load_standard` | 3.265 ms | 306 |
| `ontology_fits_check` | 63.4 ns | 15,773,000 |
| `ontology_is_subtype` | 74.8 ns | 13,369,000 |
| `ontology_mandatory_tags` | 46.3 ns | 21,598,000 |
| `ontology_validate_entity` | 205.8 ns | 4,860,000 |

**Observations:**
- Namespace loading at 3.265ms — a one-time startup cost (down from 4.7ms in v0.7.x)
- All runtime ontology lookups sub-microsecond: fits at ~15.8M ops/sec, mandatory_tags at ~21.6M ops/sec
- Entity validation at 205.8ns = ~4.86M validations/sec

### Xeto Type System

| Benchmark | Mean | Ops/sec |
|-----------|------|---------|
| `xeto_fits_ahu` | 264.7 ns | 3,778,000 |
| `xeto_fits_missing_marker` | 306.7 ns | 3,261,000 |
| `xeto_fits_explain` | 304.3 ns | 3,286,000 |
| `xeto_fits_site` | 198.9 ns | 5,028,000 |
| `xeto_effective_slots` | 114.1 ns | 8,766,000 |
| `xeto_effective_slots_inherited` | 115.4 ns | 8,666,000 |

**Observations:**
- Effective slot resolution at ~114-115ns = ~8.7M ops/sec
- Xeto fitting at ~3-5M ops/sec depending on spec complexity
- `fits_explain` (which collects issue diagnostics) costs almost the same as `fits_missing_marker` — the explanation path adds negligible overhead

### Authentication

| Benchmark | Mean | Ops/sec |
|-----------|------|---------|
| `auth_derive_credentials` | 228.6 µs | 4,374 |
| `auth_generate_nonce` | 30.5 ns | 32,787,000 |
| `auth_client_first_message` | 143.0 ns | 6,993,000 |
| `auth_parse_bearer` | 17.4 ns | 57,471,000 |
| `auth_parse_hello` | 54.8 ns | 18,248,000 |

**Observations:**
- `auth_derive_credentials` at 228.6µs uses 1,000 PBKDF2 iterations (reduced for benchmarking; production default is 100,000 iterations)
- Nonce generation at 30.5ns = ~32.8M ops/sec
- Bearer/hello parsing at 17-55ns — negligible overhead per request

### Unit Conversion

| Benchmark | Mean | Ops/sec |
|-----------|------|---------|
| `unit_convert_temperature` | 62.8 ns | 15,924,000 |
| `unit_compatible_check` | 43.4 ns | 23,041,000 |
| `unit_quantity_lookup` | 18.7 ns | 53,476,000 |

**Observations:**
- Temperature conversion (affine transform) at ~62.8ns = ~15.9M ops/sec
- Compatibility check at ~43.4ns = ~23.0M ops/sec — fast enough for inline validation
- Quantity lookup at ~18.7ns = ~53.5M ops/sec — hash table lookup

### Graph Traversal

| Benchmark | Mean | Ops/sec |
|-----------|------|---------|
| `graph_hierarchy_tree` | 23.5 µs | 42,553 |
| `graph_classify` | 42.4 ns | 23,585,000 |
| `graph_ref_chain` | 109.5 ns | 9,132,000 |
| `graph_children` | 2.305 µs | 433,839 |
| `graph_site_for` | 31.8 ns | 31,447,000 |
| `graph_equip_points` | 572.4 ns | 1,747,600 |

**Observations:**
- `site_for` at ~31.8ns = ~31.4M ops/sec — follows ref chain, returns in < 32ns
- `ref_chain` at ~109.5ns walks a 2-hop chain (point->equip->site)
- `classify` at ~42.4ns = ~23.6M ops/sec — determines entity type from markers
- `hierarchy_tree` builds a full 112-entity tree (2 sites x 5 equips x 10 points) in ~23.5µs

### Validation

| Benchmark | Mean | Ops/sec |
|-----------|------|---------|
| `validate_graph_1000` | 208.4 µs | 4,798 |

**Observations:**
- Graph validation at 208.4µs for 1000 entities = ~4.80M entities/sec
- Validates: spec conformance, tag types, dangling refs, spec coverage

---

## Server Benchmarks (haystack-server)

The historical report listed 11 HTTP benchmarks against a live Axum server with
1,000 points plus 10 sites. The harness reused clients and connections; the figures
do not establish TCP connection setup cost on every request.

### HTTP — Standard Operations

| Benchmark | Mean | Req/sec |
|-----------|------|---------|
| `http_about` | 40.2 µs | 24,876 |
| `http_read_by_id` | 45.4 µs | 22,026 |
| `http_read_filter` | 38.5 µs | 25,974 |
| `http_read_filter_large` | 2.408 ms | 415 |
| `http_nav` | 51.4 µs | 19,455 |

**Observations:**
- Single-entity read at 45.4µs = ~22K req/sec
- Filter returning ~100 entities at 38.5µs — faster than single-entity read due to batch efficiency
- Large filter (all 1000 entities) at 2.408ms — dominated by serialization time

### HTTP — History Operations

| Benchmark | Mean | Req/sec |
|-----------|------|---------|
| `http_his_read_1000` | 1.211 ms | 826 |
| `http_his_write_100` | 164.8 µs | 6,068 |

**Observations:**
- History read of 1000 items at 1.211ms; write of 100 items at 164.8µs
- Write is significantly faster per-item (~1.65µs/item vs ~1.21µs/item read) due to simpler write path

### HTTP — Watch Operations

| Benchmark | Mean | Req/sec |
|-----------|------|---------|
| `http_watch_sub` | 115.2 µs | 8,681 |
| `http_watch_poll_no_changes` | 37.7 µs | 26,525 |

**Observations:**
- Watch poll with no changes at 37.7µs — the fast path for idle watches
- Subscribe (includes sub + unsub cleanup) at 115.2µs

### HTTP — Concurrent Load

| Benchmark | Mean | Effective Req/sec |
|-----------|------|-------------------|
| `http_concurrent_reads_10` | 151.9 µs | 65,833 |
| `http_concurrent_reads_50` | 591.4 µs | 84,559 |

**Observations:**
- 10 parallel reads complete in 151.9µs = ~65.8K effective req/sec
- These are closed-loop batch completion figures. Dividing batch time by request
  count does not establish individual request latency or sustainable service capacity.

---

## Version Comparison (0.4.x -> 0.8.0)

Historical reported timings across versions. Cache state and comparable raw
provenance are missing, so these figures do not establish causal speedups.

| Benchmark | v0.4.x | v0.5.4 | v0.6.x | v0.7.0 | v0.8.0 |
|-----------|--------|--------|--------|--------|--------|
| `zinc_encode_100_rows` | 64.2 µs | 53.6 µs | 52.1 µs | 54.1 µs | 40.9 µs |
| `zinc_encode_1000_rows` | 640.1 µs | 549.2 µs | 520.1 µs | 541.3 µs | 580.4 µs |
| `filter_eval_simple` | 15.7 ns | 10.4 ns | 11.7 ns | 11.6 ns | 7.8 ns |
| `filter_eval_complex` | 84.6 ns | 51.4 ns | 57.7 ns | 55.6 ns | 34.9 ns |
| `graph_get_entity` | 22.3 ns | 16.7 ns | 18.0 ns | 18.3 ns | 7.1 ns |
| `graph_update_entity` | 32.1 µs | 7.10 µs | 6.73 µs | 1.96 µs | 1.264 µs |
| `graph_filter_10000` | — | — | 7.90 ms | 6.63 ms | 4.622 ms |
| `graph_changes_since` | — | 17.2 µs | 2.0 ns | 1.8 ns | 1.3 ns |
| `validate_graph_1000` | — | — | 502.9 µs | 343.0 µs | 208.4 µs |
| `xeto_effective_slots` | 598.1 ns | 165.0 ns | 320.9 ns | 183.9 ns | 114.1 ns |
| `graph_filter_compound_10000` | — | — | — | 12.1 µs | 54.1 ns |

The former 224× compound-filter attribution is withdrawn: an unspecified cache
state cannot establish the cost of parsing or evaluating that filter.

---

## Benchmark Data

Test datasets used in benchmarks:

| Dataset | Entity Count | Structure | Used By |
|---------|-------------|-----------|---------|
| Homogeneous 1K | 1,001 | 1 site + 1K identical points (7 tags each) | Existing graph benchmarks |
| Homogeneous 10K | 10,001 | 1 site + 10K identical points | `graph_filter_10000_entities` |
| Hierarchy 112 | 112 | 2 sites -> 10 equips -> 100 points | Traversal benchmarks |
| Historical Realistic 10K | 10,125 | 125 campuses × 81 entities with diverse tag sets | Legacy optimization benchmarks |

---

## Running Benchmarks

```bash
# All core benchmarks
cargo bench -p rusty-haystack-core

# All server benchmarks
cargo bench -p rusty-haystack-server

# Run a specific benchmark
cargo bench -p rusty-haystack-core -- graph_filter

# Run all benchmarks
cargo bench
```

---

## Summary

| Category | Highlight | Throughput |
|----------|-----------|------------|
| **Codec (Zinc encode)** | 40.9µs / 100 rows | ~2.44M rows/sec |
| **Codec (Zinc decode)** | 72.9µs / 100 rows | ~1.37M rows/sec |
| Codec (JSON v4 encode) | 87.8µs / 100 rows | ~1.14M rows/sec |
| Codec (JSON v3 encode) | 53.7µs / 100 rows | ~1.86M rows/sec |
| Graph lookup | 7.1ns per get | ~141M ops/sec |
| **Graph update (delta)** | **1.264µs per update** | **~791K ops/sec** |
| Graph add (single) | 621ns per entity | ~1.61M ops/sec |
| Graph filtering (1K) | 418.8µs per query | ~2.4K queries/sec |
| **Graph compound filter (10K)** | **54.1ns per query** | **~18.5M queries/sec** |
| Graph ref_chain | 109.5ns per walk | ~9.1M ops/sec |
| Graph site_for | 31.8ns per resolve | ~31.4M ops/sec |
| Graph hierarchy_tree | 23.5µs / 112 entities | ~42.6K trees/sec |
| Filter evaluation | 34.9ns complex eval | ~28.7M ops/sec |
| Unit conversion | 62.8ns per convert | ~15.9M ops/sec |
| Ontology fitting | 63.4ns per check | ~15.8M ops/sec |
| Xeto slot resolution | 114.1ns effective slots | ~8.8M ops/sec |
| Graph validation | 208.4µs / 1K entities | ~4.80M entities/sec |
| Dict tag lookup | 6.8ns per get | ~147M ops/sec |
| Auth credential derive | 228.6µs (1K iterations) | ~4.4K ops/sec |
| HTTP read by ID | 45.4µs per request | ~22.0K req/sec |
| HTTP filter (~100 results) | 38.5µs per request | ~26.0K req/sec |
| HTTP watch poll | 37.7µs per request | ~26.5K req/sec |
| Concurrent reads (50) | 591.4µs total | ~84.6K effective req/sec |
