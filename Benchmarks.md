# Rusty Haystack Benchmarks

## Environment

| Property | Value |
|----------|-------|
| Platform | macOS (Darwin 25.4.0, arm64) |
| CPU | Apple M2 |
| Memory | 8 GB |
| Rust | 1.93.1 |
| Version | 0.5.3 |
| Profile | release (optimized) |
| Framework | Criterion 0.5 |
| Date | 2026-03-02 |

---

## Core Operations (haystack-core)

### Codec — Encode / Decode

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `zinc_encode_scalar` | 85.4 ns | 11,709,602 | Single Number scalar |
| `zinc_decode_scalar` | 123.8 ns | 8,077,544 | Single Number scalar |
| `zinc_encode_100_rows` | 60.1 µs | 16,639 | 100-row grid, 7 columns |
| `zinc_decode_100_rows` | 103.7 µs | 9,643 | 100-row grid, 7 columns |
| `zinc_encode_1000_rows` | 591.7 µs | 1,690 | 1000-row grid, 7 columns |
| `zinc_decode_1000_rows` | 1.086 ms | 921 | 1000-row grid, 7 columns |
| `json4_encode_100_rows` | 135.7 µs | 7,369 | JSON v4, 100 rows |
| `json4_decode_100_rows` | 194.5 µs | 5,141 | JSON v4, 100 rows |
| `json4_encode_1000_rows` | 1.356 ms | 737 | JSON v4, 1000 rows |
| `json4_decode_1000_rows` | 1.952 ms | 512 | JSON v4, 1000 rows |
| `csv_encode_1000_rows` | 784.2 µs | 1,275 | CSV, 1000 rows (encode only) |
| `codec_roundtrip_mixed_types` | 421.3 µs | 2,374 | Zinc encode+decode, 100 rows, 9 mixed types |

**Observations:**
- Zinc is ~2.3x faster than JSON for encoding and ~1.9x for decoding at 100 rows
- Zinc encode throughput: ~1,690 grids/sec for 1000 rows (~1.69M rows/sec)
- Zinc decode throughput: ~921 grids/sec for 1000 rows (~921k rows/sec)
- Scalar encode/decode improved ~12%/~34% from 0.4.x (85ns/124ns vs 97ns/186ns)
- Mixed-type roundtrip improved ~7% (421µs vs 454µs)

### Filter — Parse & Evaluate

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `filter_parse_simple` | 87.1 ns | 11,481,056 | Parse `site` |
| `filter_parse_complex` | 583.0 ns | 1,715,266 | Parse `site and equip and point and temp > 70°F` |
| `filter_eval_simple` | 11.6 ns | 86,206,897 | Evaluate marker check |
| `filter_eval_complex` | 55.5 ns | 18,018,018 | Evaluate 4-clause filter with comparison |

**Observations:**
- Filter evaluation is extremely fast (sub-100ns even for complex filters)
- Simple filter evaluation: ~86.2M ops/sec (↑35% from 63.7M)
- Complex 4-clause evaluation: ~18.0M ops/sec (↑52% from 11.8M)
- Parsing is ~6.7x slower for complex multi-clause filters vs simple tag checks

### Graph — Entity Operations

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `graph_get_entity` | 18.6 ns | 53,763,441 | Get by ID from 1000-entity graph |
| `graph_add_entity` | 833.5 ns | 1,199,760 | Add single entity |
| `graph_add_1000_entities` | 1.809 ms | 553 | Bulk insert 1000 entities into fresh graph |
| `graph_update_entity` | 6.55 µs | 152,672 | Update entity in 1000-entity graph |
| `graph_remove_entity` | 1.97 µs | 507,614 | Remove + re-add cycle |
| `graph_filter_1000_entities` | 608.7 µs | 1,643 | Filter 1000 entities (`point and temp > 70°F`) |
| `graph_filter_10000_entities` | 6.534 ms | 153 | Filter 10,000 entities (same filter) |
| `graph_changes_since` | 17.4 µs | 57,471 | Query changelog at midpoint version |
| `shared_graph_concurrent_rw` | 270.1 µs | 3,702 | 4 reader threads + 1 writer thread, 100 entities |

**Observations:**
- Entity lookup is O(1) at ~19ns via HashMap (~53.8M ops/sec)
- Graph filtering improved ~16%: 1k entities 609µs (vs 727µs), 10k entities 6.5ms (vs 8.0ms)
- Bulk insert of 1000 entities averages ~1.8µs per entity (~553k entities/sec)
- Graph update improved ~80% (6.5µs vs 32.1µs) from optimized index maintenance
- Concurrent SharedGraph contention (4 readers + 1 writer, 10 ops each) takes ~270µs total

### Ontology — Namespace Operations

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `ontology_load_standard` | 4.657 ms | 215 | Load standard namespace from bundled data |
| `ontology_fits_check` | 98.9 ns | 10,111,223 | Check if entity fits `ahu` type |
| `ontology_is_subtype` | 103.8 ns | 9,633,912 | Check `ahu` is subtype of `equip` |
| `ontology_mandatory_tags` | 69.9 ns | 14,306,152 | Get mandatory tags for `ahu` |
| `ontology_validate_entity` | 339.6 ns | 2,944,622 | Validate entity against ontology |

**Observations:**
- Namespace loading (~4.7ms) is a one-time startup cost (~3% faster)
- All runtime ontology lookups are sub-microsecond (~10-14M ops/sec)
- Validation improved ~4% (340ns vs 353ns)

### Xeto — Structural Type Fitting

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `xeto_fits_ahu` | 400.4 ns | 2,497,503 | Fits check: entity with ahu+equip markers |
| `xeto_fits_missing_marker` | 462.4 ns | 2,162,630 | Fits check: entity missing required marker (fail path) |
| `xeto_fits_explain` | 462.5 ns | 2,162,162 | Fits with issue explanation (fail path) |
| `xeto_fits_site` | 309.6 ns | 3,229,974 | Fits check: simple site entity |
| `xeto_effective_slots` | 311.1 ns | 3,214,401 | Resolve effective slots for a spec |
| `xeto_effective_slots_inherited` | 323.4 ns | 3,092,146 | Resolve effective slots with base chain |

**Observations:**
- Xeto fitting is sub-microsecond for all cases (~2-3M ops/sec)
- Simpler specs (site) are ~23% faster than complex ones (ahu)
- Effective slot resolution improved ~48% (311ns vs 598ns) from optimized inheritance chain
- Slot resolution with inheritance chain: 323ns (vs 602ns, ↑46%)

---

## Server Operations (haystack-server)

Real HTTP benchmarks against a live server (actix-web) with 1000 pre-loaded entities across 10 sites. Each request includes full HTTP round-trip (TCP, serialize, deserialize).

### HTTP — Standard Operations

| Benchmark | Mean | Req/sec | Description |
|-----------|------|---------|-------------|
| `http_about` | 56.5 µs | 17,699 | Server info endpoint |
| `http_read_by_id` | 59.5 µs | 16,807 | Read single entity by ID |
| `http_read_filter` | 501.8 µs | 1,993 | Filter returning ~100 entities (`siteRef==@site-0`) |
| `http_read_filter_large` | 3.699 ms | 270 | Filter returning all 1000 entities |
| `http_nav` | 74.3 µs | 13,459 | Navigation tree root |

**Observations:**
- About endpoint: ~57µs (↑51% from 115µs, ~17.7k req/sec)
- Single entity read: ~60µs (↑47% from 111µs, ~16.8k req/sec)
- Filtering ~100 entities: ~502µs (↑39% from 820µs, ~2.0k req/sec)
- Large result set (1000 entities): ~3.7ms (↑14% from 4.3ms, ~270 req/sec)
- Navigation tree: ~74µs (~13.5k req/sec)

### HTTP — History Operations

| Benchmark | Mean | Req/sec | Description |
|-----------|------|---------|-------------|
| `http_his_read_1000` | 1.957 ms | 511 | Read 1000 history items for a point |
| `http_his_write_100` | 259.8 µs | 3,849 | Write 100 history items |

**Observations:**
- History read of 1000 items: ~1.96ms (↑16% from 2.34ms, ~511 req/sec)
- History write of 100 items: ~260µs (↑45% from 469µs, ~3.8k req/sec, ~2.6µs per item)

### HTTP — Watch Operations

| Benchmark | Mean | Req/sec | Description |
|-----------|------|---------|-------------|
| `http_watch_sub` | 166.4 µs | 6,010 | Subscribe to 10 entities (includes sub + unsub cleanup) |
| `http_watch_poll_no_changes` | 48.0 µs | 20,833 | Poll existing watch, no changes (fast path) |

**Observations:**
- Watch poll with no changes: ~48µs (~20.8k req/sec)
- Watch subscribe + unsubscribe cycle: ~166µs (~6.0k req/sec)

### HTTP — Concurrent Load

| Benchmark | Mean | Effective Req/sec | Description |
|-----------|------|-------------------|-------------|
| `http_concurrent_reads_10` | 193.3 µs | 51,733 | 10 parallel HTTP reads by ID |
| `http_concurrent_reads_50` | 729.1 µs | 68,576 | 50 parallel HTTP reads by ID |

**Observations:**
- 10 concurrent reads: ~193µs total (~19µs effective per request, ~51.7k effective req/sec)
- 50 concurrent reads: ~729µs total (~15µs effective per request, ~68.6k effective req/sec)
- Near-linear scaling: 50 clients = ~3.8x the time of 10 clients (5x the load)
- Effective throughput improved ~25% (68.6k vs 54.9k effective req/sec at 50 clients)

---

## Federation Operations (haystack-server)

Federation benchmarks using 3 in-process servers: 1 lead server with 2 federated remotes (10,000 entities each, 20,200 total across federation). Remote servers run with SCRAM SHA-256 auth enabled. Proxy operations (hisRead, hisWrite, pointWrite) include a full SCRAM handshake + HTTP round-trip to the owning remote.

### Sync — Fetch All Entities from Remotes

| Benchmark | Mean | Description |
|-----------|------|-------------|
| `read_all_10k_from_remote` | 42.5 ms | Read all 10,100 entities from one remote |
| `read_all_20k_both_remotes` | 64.5 ms | Read all entities from both remotes concurrently |

**Observations:**
- Single remote sync (10k entities): ~43ms (~235k entities/sec)
- Concurrent dual-remote sync: ~65ms (only ~1.5x single, good parallelism)

### Federated Reads — Bitmap-Indexed Cache

| Benchmark | Mean | Req/sec | Description |
|-----------|------|---------|-------------|
| `read_by_id` | 63.4 µs | 15,773 | Read single federated entity by prefixed ID |
| `filter_site` | 75.2 µs | 13,298 | Filter `site and dis=="Site 5"` across federation |
| `filter_all_points_20k` | 79.1 ms | 13 | Filter all 20k federated points |
| `nav_root` | 47.3 µs | 21,142 | Navigation tree root (local only) |

**Observations:**
- Federated read-by-id: ~63µs (↑**97x** from 6.13ms) — bloom filter + bitmap index eliminate cache scanning
- Federated filter: ~75µs (↑**182x** from 13.65ms) — bitmap-indexed connector caches match local performance
- Filtering 20k federated entities: ~79ms (~253k entities/sec filtering throughput, ↑3%)
- Nav root: ~47µs (~21.1k req/sec), unchanged (local-only path)

### Federated Write Proxy — Persistent Connection to Remote

| Benchmark | Mean | Req/sec | Description |
|-----------|------|---------|-------------|
| `his_read_proxied_1000` | 3.765 ms | 266 | hisRead 1000 items, proxied through lead to remote |
| `his_write_proxied_100` | 517.6 µs | 1,932 | hisWrite 100 items, proxied through lead to remote |
| `point_write_proxied` | 114.2 µs | 8,757 | pointWrite, proxied through lead to remote |

**Observations:**
- Proxy operations reuse the persistent authenticated connection (no per-request SCRAM handshake)
- hisRead (1000 items): ~3.8ms, dominated by timestamp serialization (~266 req/sec, ↑3%)
- hisWrite (100 items): ~518µs (~1.9k req/sec, ~5.2µs per item)
- pointWrite: ~114µs (↑21% from 145µs, ~8.8k req/sec)

### Federated Concurrent Load

| Benchmark | Mean | Effective Req/sec | Description |
|-----------|------|-------------------|-------------|
| `concurrent_reads_50` | 760.1 µs | 65,781 | 50 parallel federated read-by-id requests |
| `concurrent_filter_50` | 22.4 ms | 2,232 | 50 parallel federated filter reads (`siteRef==@ra-site-N`) |

**Observations:**
- 50 concurrent federated reads: ~760µs total (~15µs effective per request, ~65.8k effective req/sec)
- 50 concurrent filter reads: ~22ms total (~449µs effective per request, ~2.2k effective req/sec)
- Federated concurrent reads improved **147x** (760µs vs 111.7ms) — now comparable to local HTTP performance
- Federated concurrent filters improved **10x** (22ms vs 230ms)

---

## Version Comparison (0.4.x → 0.5.3)

Key improvements from graph/federation optimizations, bitmap-indexed connector caches, bloom filters, and codec refinements:

| Benchmark | v0.4.x | v0.5.3 | Improvement |
|-----------|--------|--------|-------------|
| `zinc_decode_scalar` | 186.4 ns | 123.8 ns | **↑34%** |
| `filter_eval_simple` | 15.7 ns | 11.6 ns | **↑26%** |
| `filter_eval_complex` | 84.6 ns | 55.5 ns | **↑34%** |
| `graph_update_entity` | 32.1 µs | 6.55 µs | **↑80%** |
| `graph_filter_1000` | 726.6 µs | 608.7 µs | **↑16%** |
| `xeto_effective_slots` | 598.1 ns | 311.1 ns | **↑48%** |
| `http_about` | 115.2 µs | 56.5 µs | **↑51%** |
| `http_read_by_id` | 111.3 µs | 59.5 µs | **↑47%** |
| `http_his_write_100` | 468.6 µs | 259.8 µs | **↑45%** |
| `fed. read_by_id` | 6.13 ms | 63.4 µs | **↑97x** |
| `fed. filter_site` | 13.65 ms | 75.2 µs | **↑182x** |
| `fed. concurrent_reads_50` | 111.7 ms | 760.1 µs | **↑147x** |
| `fed. concurrent_filter_50` | 229.6 ms | 22.4 ms | **↑10x** |

---

## Summary

| Category | Highlight | Throughput |
|----------|-----------|------------|
| Codec (Zinc encode) | 60.1µs / 100 rows | ~1.69M rows/sec |
| Codec (Zinc decode) | 103.7µs / 100 rows | ~921k rows/sec |
| Graph lookup | 18.6ns per get | ~53.8M ops/sec |
| Graph filtering | 608.7µs / 1000 entities | ~1.6k queries/sec |
| Filter evaluation | 55.5ns complex eval | ~18.0M ops/sec |
| Ontology fitting | Sub-microsecond | ~10M ops/sec |
| HTTP read by ID | 59.5µs per request | ~16.8k req/sec |
| HTTP filter (100 results) | 501.8µs per request | ~2.0k req/sec |
| HTTP nav | 74.3µs per request | ~13.5k req/sec |
| HTTP watch poll | 48.0µs per request | ~20.8k req/sec |
| Concurrent reads (50) | 729µs total | ~68.6k effective req/sec |
| History read (1000 items) | 1.957ms per request | ~511 req/sec |
| Fed. sync (10k entities) | 42.5ms per remote | ~235k entities/sec |
| Fed. read by ID | 63.4µs per request | ~15.8k req/sec |
| Fed. filter (20k entities) | 79.1ms per request | ~13 req/sec |
| Fed. proxy hisRead (1000) | 3.765ms per request | ~266 req/sec |
| Fed. proxy pointWrite | 114.2µs per request | ~8.8k req/sec |
| Fed. concurrent reads (50) | 760µs total | ~65.8k effective req/sec |
