# Rusty Haystack Benchmarks

## Environment

| Property | Value |
|----------|-------|
| Platform | macOS (Darwin 25.4.0, arm64) |
| CPU | Apple M2 |
| Memory | 8 GB |
| Rust | 1.93.1 |
| Profile | release (optimized) |
| Framework | Criterion 0.5 |
| Date | 2026-03-01 |

---

## Core Operations (haystack-core)

### Codec — Encode / Decode

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `zinc_encode_scalar` | 96.7 ns | 10,341,262 | Single Number scalar |
| `zinc_decode_scalar` | 186.4 ns | 5,364,807 | Single Number scalar |
| `zinc_encode_100_rows` | 64.4 us | 15,528 | 100-row grid, 7 columns |
| `zinc_decode_100_rows` | 118.9 us | 8,410 | 100-row grid, 7 columns |
| `zinc_encode_1000_rows` | 1.029 ms | 972 | 1000-row grid, 7 columns |
| `zinc_decode_1000_rows` | 2.103 ms | 476 | 1000-row grid, 7 columns |
| `json4_encode_100_rows` | 150.1 us | 6,662 | JSON v4, 100 rows |
| `json4_decode_100_rows` | 386.3 us | 2,589 | JSON v4, 100 rows |
| `json4_encode_1000_rows` | 1.546 ms | 647 | JSON v4, 1000 rows |
| `json4_decode_1000_rows` | 2.118 ms | 472 | JSON v4, 1000 rows |
| `csv_encode_1000_rows` | 827.9 us | 1,208 | CSV, 1000 rows (encode only) |
| `codec_roundtrip_mixed_types` | 454.0 us | 2,203 | Zinc encode+decode, 100 rows, 9 mixed types |

**Observations:**
- Zinc is ~2x faster than JSON for encoding and ~3x for decoding at 100 rows
- Zinc encode throughput: ~972 grids/sec for 1000 rows (~972k rows/sec)
- Zinc decode throughput: ~476 grids/sec for 1000 rows (~476k rows/sec)
- Mixed-type roundtrip (9 types including DateTime, Uri, Bool) adds overhead vs homogeneous data

### Filter — Parse & Evaluate

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `filter_parse_simple` | 91.6 ns | 10,917,031 | Parse `site` |
| `filter_parse_complex` | 589.0 ns | 1,697,793 | Parse `site and equip and point and temp > 70°F` |
| `filter_eval_simple` | 15.7 ns | 63,694,268 | Evaluate marker check |
| `filter_eval_complex` | 84.6 ns | 11,820,331 | Evaluate 4-clause filter with comparison |

**Observations:**
- Filter evaluation is extremely fast (sub-100ns even for complex filters)
- Simple filter evaluation: ~63.7M ops/sec
- Complex 4-clause evaluation: ~11.8M ops/sec
- Parsing is ~6.4x slower for complex multi-clause filters vs simple tag checks

### Graph — Entity Operations

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `graph_get_entity` | 11.3 ns | 88,495,575 | Get by ID from 1000-entity graph |
| `graph_add_entity` | 646.7 ns | 1,546,305 | Add single entity |
| `graph_add_1000_entities` | 1.905 ms | 525 | Bulk insert 1000 entities into fresh graph |
| `graph_update_entity` | 32.1 us | 31,153 | Update entity in 1000-entity graph |
| `graph_remove_entity` | 1.80 us | 555,556 | Remove + re-add cycle |
| `graph_filter_1000_entities` | 726.6 us | 1,376 | Filter 1000 entities (`point and temp > 70°F`) |
| `graph_filter_10000_entities` | 7.956 ms | 126 | Filter 10,000 entities (same filter) |
| `graph_changes_since` | 14.9 ns | 67,114,094 | Query changelog at midpoint version |
| `shared_graph_concurrent_rw` | 275.8 us | 3,626 | 4 reader threads + 1 writer thread, 100 entities |

**Observations:**
- Entity lookup is O(1) at ~11ns via HashMap (~88.5M ops/sec)
- Graph filtering scales linearly: 10k entities = ~10.9x the time of 1k entities
- Bulk insert of 1000 entities averages ~1.9us per entity (~525k entities/sec)
- Changelog query is near-instant at ~15ns (~67M ops/sec)
- Concurrent SharedGraph contention (4 readers + 1 writer, 10 ops each) takes ~276us total

### Ontology — Namespace Operations

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `ontology_load_standard` | 4.809 ms | 208 | Load standard namespace from bundled data |
| `ontology_fits_check` | 100.2 ns | 9,980,040 | Check if entity fits `ahu` type |
| `ontology_is_subtype` | 106.3 ns | 9,407,261 | Check `ahu` is subtype of `equip` |
| `ontology_mandatory_tags` | 72.8 ns | 13,736,264 | Get mandatory tags for `ahu` |
| `ontology_validate_entity` | 353.4 ns | 2,829,654 | Validate entity against ontology |

**Observations:**
- Namespace loading (~4.8ms) is a one-time startup cost
- All runtime ontology lookups are sub-microsecond (~10-14M ops/sec)

### Xeto — Structural Type Fitting

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `xeto_fits_ahu` | 404.1 ns | 2,474,636 | Fits check: entity with ahu+equip markers |
| `xeto_fits_missing_marker` | 466.0 ns | 2,145,923 | Fits check: entity missing required marker (fail path) |
| `xeto_fits_explain` | 467.6 ns | 2,138,578 | Fits with issue explanation (fail path) |
| `xeto_fits_site` | 306.0 ns | 3,267,974 | Fits check: simple site entity |
| `xeto_effective_slots` | 598.1 ns | 1,671,960 | Resolve effective slots for a spec |
| `xeto_effective_slots_inherited` | 601.9 ns | 1,661,405 | Resolve effective slots with base chain |

**Observations:**
- Xeto fitting is sub-microsecond for all cases (~2-3M ops/sec)
- Simpler specs (site) are ~24% faster than complex ones (ahu)
- Fail-path (missing marker) takes only ~15% longer than success path
- Slot resolution with inheritance has negligible overhead vs direct slots

---

## Server Operations (haystack-server)

Real HTTP benchmarks against a live server (actix-web) with 1000 pre-loaded entities across 10 sites. Each request includes full HTTP round-trip (TCP, serialize, deserialize).

### HTTP — Standard Operations

| Benchmark | Mean | Req/sec | Description |
|-----------|------|---------|-------------|
| `http_about` | 115.2 us | 8,681 | Server info endpoint |
| `http_read_by_id` | 111.3 us | 8,985 | Read single entity by ID |
| `http_read_filter` | 820.1 us | 1,219 | Filter returning ~100 entities (`siteRef==@site-0`) |
| `http_read_filter_large` | 4.293 ms | 233 | Filter returning all 1000 entities |
| `http_nav` | 65.4 us | 15,291 | Navigation tree root |

**Observations:**
- Navigation is the fastest endpoint at ~65us (~15.3k req/sec)
- Single entity read: ~111us (~9.0k req/sec)
- Filtering ~100 entities: ~820us (~1.2k req/sec, includes server-side filtering + serialization)
- Large result set (1000 entities) dominated by serialization at ~4.3ms (~233 req/sec)

### HTTP — History Operations

| Benchmark | Mean | Req/sec | Description |
|-----------|------|---------|-------------|
| `http_his_read_1000` | 2.336 ms | 428 | Read 1000 history items for a point |
| `http_his_write_100` | 468.6 us | 2,134 | Write 100 history items |

**Observations:**
- History read of 1000 items: ~2.3ms (~428 req/sec, includes timestamp serialization)
- History write of 100 items: ~469us (~2.1k req/sec, ~4.7us per item)

### HTTP — Watch Operations

| Benchmark | Mean | Req/sec | Description |
|-----------|------|---------|-------------|
| `http_watch_sub` | 148.9 us | 6,716 | Subscribe to 10 entities (includes sub + unsub cleanup) |
| `http_watch_poll_no_changes` | 42.6 us | 23,474 | Poll existing watch, no changes (fast path) |

**Observations:**
- Watch poll with no changes is the fastest operation (~43us, ~23.5k req/sec)
- Watch subscribe + unsubscribe cycle: ~149us (~6.7k req/sec)

### HTTP — Concurrent Load

| Benchmark | Mean | Effective Req/sec | Description |
|-----------|------|-------------------|-------------|
| `http_concurrent_reads_10` | 229.2 us | 43,627 | 10 parallel HTTP reads by ID |
| `http_concurrent_reads_50` | 910.0 us | 54,945 | 50 parallel HTTP reads by ID |

**Observations:**
- 10 concurrent reads: ~229us total (~23us effective per request, ~43.6k effective req/sec)
- 50 concurrent reads: ~910us total (~18us effective per request, ~54.9k effective req/sec)
- Near-linear scaling: 50 clients = ~4.0x the time of 10 clients (5x the load)
- The server handles concurrent reads efficiently with minimal contention

---

## Federation Operations (haystack-server)

Federation benchmarks using 3 in-process servers: 1 lead server with 2 federated remotes (10,000 entities each, 20,200 total across federation). Remote servers run with SCRAM SHA-256 auth enabled. Proxy operations (hisRead, hisWrite, pointWrite) include a full SCRAM handshake + HTTP round-trip to the owning remote.

### Sync — Fetch All Entities from Remotes

| Benchmark | Mean | Description |
|-----------|------|-------------|
| `read_all_10k_from_remote` | 42.1 ms | Read all 10,100 entities from one remote |
| `read_all_20k_both_remotes` | 64.9 ms | Read all entities from both remotes concurrently |

**Observations:**
- Single remote sync (10k entities): ~42ms (~240k entities/sec)
- Concurrent dual-remote sync: ~65ms (only ~1.5x single, good parallelism)

### Federated Reads — Cache-Based Merging

| Benchmark | Mean | Req/sec | Description |
|-----------|------|---------|-------------|
| `read_by_id` | 6.13 ms | 163 | Read single federated entity by prefixed ID |
| `filter_site` | 13.65 ms | 73 | Filter `site and dis=="Site 5"` across federation |
| `filter_all_points_20k` | 81.7 ms | 12 | Filter all 20k federated points |
| `nav_root` | 47.4 us | 21,097 | Navigation tree root (local only) |

**Observations:**
- Federated read-by-id (~6.1ms) is ~55x slower than local read-by-id (~111us) due to federation cache search + Zinc encoding of merged results
- Filtering 20k federated entities: ~82ms (~245k entities/sec filtering throughput)
- Nav root is fast (~47us) since it doesn't traverse federated caches

### Federated Write Proxy — Persistent Connection to Remote

| Benchmark | Mean | Req/sec | Description |
|-----------|------|---------|-------------|
| `his_read_proxied_1000` | 3.89 ms | 257 | hisRead 1000 items, proxied through lead to remote |
| `his_write_proxied_100` | 503 us | 1,988 | hisWrite 100 items, proxied through lead to remote |
| `point_write_proxied` | 145 us | 6,897 | pointWrite, proxied through lead to remote |

**Observations:**
- Proxy operations reuse the persistent authenticated connection (no per-request SCRAM handshake)
- hisRead (1000 items): ~3.9ms, dominated by timestamp serialization (~257 req/sec)
- hisWrite (100 items): ~503us (~2.0k req/sec, ~5us per item)
- pointWrite: ~145us (~6.9k req/sec), comparable to local HTTP operations

### Federated Concurrent Load

| Benchmark | Mean | Effective Req/sec | Description |
|-----------|------|-------------------|-------------|
| `concurrent_reads_50` | 111.7 ms | 448 | 50 parallel federated read-by-id requests |
| `concurrent_filter_50` | 229.6 ms | 218 | 50 parallel federated filter reads (`siteRef==@ra-site-N`) |

**Observations:**
- 50 concurrent federated reads: ~112ms total (~2.2ms effective per request, ~448 effective req/sec)
- 50 concurrent filter reads: ~230ms total (~4.6ms effective per request, ~218 effective req/sec)
- Good parallelism — 50 concurrent reads take only ~16.6x the time of a single read

---

## Summary

| Category | Highlight | Throughput |
|----------|-----------|------------|
| Codec (Zinc encode) | 64.4us / 100 rows | ~972k rows/sec |
| Codec (Zinc decode) | 118.9us / 100 rows | ~476k rows/sec |
| Graph lookup | 11.3ns per get | ~88.5M ops/sec |
| Graph filtering | 726.6us / 1000 entities | ~1.4k queries/sec |
| Filter evaluation | 84.6ns complex eval | ~11.8M ops/sec |
| Ontology fitting | Sub-microsecond | ~10M ops/sec |
| HTTP read by ID | 111.3us per request | ~9.0k req/sec |
| HTTP filter (100 results) | 820.1us per request | ~1.2k req/sec |
| HTTP nav | 65.4us per request | ~15.3k req/sec |
| HTTP watch poll | 42.6us per request | ~23.5k req/sec |
| Concurrent reads (50) | 910us total | ~54.9k effective req/sec |
| History read (1000 items) | 2.336ms per request | ~428 req/sec |
| Fed. sync (10k entities) | 42.1ms per remote | ~240k entities/sec |
| Fed. read by ID | 6.13ms per request | ~163 req/sec |
| Fed. filter (20k entities) | 81.7ms per request | ~12 req/sec |
| Fed. proxy hisRead (1000) | 3.89ms per request | ~257 req/sec |
| Fed. proxy pointWrite | 145us per request | ~6.9k req/sec |
| Fed. concurrent reads (50) | 111.7ms total | ~448 effective req/sec |
