# Rusty Haystack Benchmarks

## Environment

| Property | Value |
|----------|-------|
| Platform | macOS (Darwin 25.4.0, arm64) |
| CPU | Apple M2 |
| Memory | 8 GB |
| Rust | 1.93.1 |
| Version | 0.5.4 |
| Profile | release (optimized) |
| Framework | Criterion 0.5 |
| Date | 2026-03-02 |

---

## Core Operations (haystack-core)

### Codec — Encode / Decode

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `zinc_encode_scalar` | 89.2 ns | 11,210,762 | Single Number scalar |
| `zinc_decode_scalar` | 129.2 ns | 7,739,938 | Single Number scalar |
| `zinc_encode_100_rows` | 53.6 µs | 18,657 | 100-row grid, 7 columns |
| `zinc_decode_100_rows` | 105.1 µs | 9,515 | 100-row grid, 7 columns |
| `zinc_encode_1000_rows` | 549.2 µs | 1,821 | 1000-row grid, 7 columns |
| `zinc_decode_1000_rows` | 1.105 ms | 905 | 1000-row grid, 7 columns |
| `json4_encode_100_rows` | 146.7 µs | 6,816 | JSON v4, 100 rows |
| `json4_decode_100_rows` | 203.0 µs | 4,926 | JSON v4, 100 rows |
| `json4_encode_1000_rows` | 1.412 ms | 708 | JSON v4, 1000 rows |
| `json4_decode_1000_rows` | 2.062 ms | 485 | JSON v4, 1000 rows |
| `csv_encode_1000_rows` | 840.9 µs | 1,189 | CSV, 1000 rows (encode only) |
| `codec_roundtrip_mixed_types` | 408.0 µs | 2,451 | Zinc encode+decode, 100 rows, 9 mixed types |

**Observations:**
- Zinc 100-row encode improved **↑11%** (53.6µs vs 60.1µs) from direct write!() buffer encoding
- Zinc 1000-row encode improved **↑7%** (549µs vs 592µs) — eliminates intermediate Vec<String> allocations
- Mixed-type roundtrip improved **↑3%** (408µs vs 421µs)
- Zinc is ~2.7x faster than JSON for encoding and ~1.9x for decoding at 100 rows

### Filter — Parse & Evaluate

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `filter_parse_simple` | 89.3 ns | 11,198,208 | Parse `site` |
| `filter_parse_complex` | 591.5 ns | 1,690,616 | Parse `site and equip and point and temp > 70°F` |
| `filter_eval_simple` | 10.4 ns | 96,153,846 | Evaluate marker check |
| `filter_eval_complex` | 51.4 ns | 19,455,253 | Evaluate 4-clause filter with comparison |

**Observations:**
- Simple filter evaluation improved **↑12%**: ~96.2M ops/sec (vs 86.2M in v0.5.3)
- Complex 4-clause evaluation improved **↑8%**: ~19.5M ops/sec (vs 18.0M)
- AST caching eliminates re-parsing overhead for repeated queries (not measured in microbenchmarks)

### Graph — Entity Operations

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `graph_get_entity` | 16.7 ns | 59,880,240 | Get by ID from 1000-entity graph |
| `graph_add_entity` | 896.7 ns | 1,115,194 | Add single entity |
| `graph_add_1000_entities` | 2.147 ms | 466 | Bulk insert 1000 entities into fresh graph |
| `graph_update_entity` | 7.10 µs | 140,845 | Update entity in 1000-entity graph |
| `graph_remove_entity` | 2.86 µs | 349,650 | Remove + re-add cycle |
| `graph_filter_1000_entities` | 632.3 µs | 1,582 | Filter 1000 entities (`point and temp > 70°F`) |
| `graph_filter_10000_entities` | 7.275 ms | 137 | Filter 10,000 entities (same filter) |
| `graph_changes_since` | 17.2 µs | 58,140 | Query changelog at midpoint version |
| `shared_graph_concurrent_rw` | 289.0 µs | 3,460 | 4 reader threads + 1 writer thread, 100 entities |

**Observations:**
- Entity lookup improved **↑10%**: ~59.9M ops/sec (vs 53.8M) — tighter HashMap path
- Add/update/remove show slight overhead from auto-indexing 8 fields on mutation (expected trade-off for faster queries)
- Graph filtering within noise range; real-world gains come from bitmap/adjacency acceleration on ref queries (e.g. `siteRef==@site-0`)

### Ontology — Namespace Operations

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `ontology_load_standard` | 4.773 ms | 210 | Load standard namespace from bundled data |
| `ontology_fits_check` | 100.4 ns | 9,960,159 | Check if entity fits `ahu` type |
| `ontology_is_subtype` | 109.3 ns | 9,149,131 | Check `ahu` is subtype of `equip` |
| `ontology_mandatory_tags` | 71.9 ns | 13,908,206 | Get mandatory tags for `ahu` |
| `ontology_validate_entity` | 353.4 ns | 2,829,654 | Validate entity against ontology |

**Observations:**
- Namespace loading (~4.8ms) is a one-time startup cost
- All runtime ontology lookups remain sub-microsecond (~10-14M ops/sec)

### Xeto — Structural Type Fitting

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `xeto_fits_ahu` | 405.1 ns | 2,468,526 | Fits check: entity with ahu+equip markers |
| `xeto_fits_missing_marker` | 469.1 ns | 2,131,741 | Fits check: entity missing required marker (fail path) |
| `xeto_fits_explain` | 464.4 ns | 2,153,316 | Fits with issue explanation (fail path) |
| `xeto_fits_site` | 305.7 ns | 3,271,181 | Fits check: simple site entity |
| `xeto_effective_slots` | 165.0 ns | 6,060,606 | Resolve effective slots for a spec |
| `xeto_effective_slots_inherited` | 168.8 ns | 5,924,171 | Resolve effective slots with base chain |

**Observations:**
- Effective slot resolution improved **↑47%** (165ns vs 311ns) — now ~6.1M ops/sec
- Inherited slots improved **↑48%** (169ns vs 323ns) — optimized inheritance chain
- Xeto fitting stable at ~2-3M ops/sec

---

## Server Operations (haystack-server)

Real HTTP benchmarks against a live server (actix-web) with 1000 pre-loaded entities across 10 sites. Each request includes full HTTP round-trip (TCP, serialize, deserialize).

### HTTP — Standard Operations

| Benchmark | Mean | Req/sec | Description |
|-----------|------|---------|-------------|
| `http_about` | 54.9 µs | 18,215 | Server info endpoint |
| `http_read_by_id` | 58.6 µs | 17,065 | Read single entity by ID |
| `http_read_filter` | 459.1 µs | 2,178 | Filter returning ~100 entities (`siteRef==@site-0`) |
| `http_read_filter_large` | 3.404 ms | 294 | Filter returning all 1000 entities |
| `http_nav` | 73.5 µs | 13,605 | Navigation tree root |

**Observations:**
- Filter reads improved **↑9%** (459µs vs 502µs) — bitmap acceleration + &str column discovery
- Large filter improved **↑8%** (3.4ms vs 3.7ms) — direct write!() buffer encoding saves allocations
- Read by ID improved **↑2%** (58.6µs vs 59.5µs) — &str column discovery eliminates String clones
- About improved **↑3%** (54.9µs vs 56.5µs)

### HTTP — History Operations

| Benchmark | Mean | Req/sec | Description |
|-----------|------|---------|-------------|
| `http_his_read_1000` | 1.900 ms | 526 | Read 1000 history items for a point |
| `http_his_write_100` | 254.8 µs | 3,925 | Write 100 history items |

**Observations:**
- History read improved **↑3%** (1.90ms vs 1.96ms, ~526 req/sec)
- History write improved **↑2%** (255µs vs 260µs, ~3.9k req/sec)

### HTTP — Watch Operations

| Benchmark | Mean | Req/sec | Description |
|-----------|------|---------|-------------|
| `http_watch_sub` | 162.4 µs | 6,158 | Subscribe to 10 entities (includes sub + unsub cleanup) |
| `http_watch_poll_no_changes` | 48.4 µs | 20,661 | Poll existing watch, no changes (fast path) |

**Observations:**
- Watch subscribe improved **↑2%** (~162µs, ~6.2k req/sec)
- Watch poll stable at ~48µs (~20.7k req/sec)

### HTTP — Concurrent Load

| Benchmark | Mean | Effective Req/sec | Description |
|-----------|------|-------------------|-------------|
| `http_concurrent_reads_10` | 187.9 µs | 53,219 | 10 parallel HTTP reads by ID |
| `http_concurrent_reads_50` | 721.7 µs | 69,277 | 50 parallel HTTP reads by ID |

**Observations:**
- 10 concurrent reads improved **↑3%**: ~188µs total (~53.2k effective req/sec)
- 50 concurrent reads improved **↑1%**: ~722µs total (~69.3k effective req/sec)
- Near-linear scaling maintained: 50 clients = ~3.8x the time of 10 clients

---

## Federation Operations (haystack-server)

Federation benchmarks using 3 in-process servers: 1 lead server with 2 federated remotes (10,000 entities each, 20,200 total across federation). Remote servers run with SCRAM SHA-256 auth enabled. Proxy operations (hisRead, hisWrite, pointWrite) include a full SCRAM handshake + HTTP round-trip to the owning remote.

### Sync — Fetch All Entities from Remotes

| Benchmark | Mean | Description |
|-----------|------|-------------|
| `read_all_10k_from_remote` | 40.8 ms | Read all 10,100 entities from one remote |
| `read_all_20k_both_remotes` | 62.8 ms | Read all entities from both remotes concurrently |

**Observations:**
- Single remote sync improved **↑4%** (40.8ms vs 42.5ms, ~248k entities/sec)
- Concurrent dual-remote sync improved **↑3%** (62.8ms vs 64.5ms, good parallelism)

### Federated Reads — Bitmap-Indexed Cache

| Benchmark | Mean | Req/sec | Description |
|-----------|------|---------|-------------|
| `read_by_id` | 60.9 µs | 16,420 | Read single federated entity by prefixed ID |
| `filter_site` | 72.8 µs | 13,736 | Filter `site and dis=="Site 5"` across federation |
| `filter_all_points_20k` | 76.3 ms | 13 | Filter all 20k federated points |
| `nav_root` | 47.5 µs | 21,053 | Navigation tree root (local only) |

**Observations:**
- Federated read-by-id improved **↑4%** (60.9µs vs 63.4µs) — consolidated single-lock CacheState
- Federated filter improved **↑3%** (72.8µs vs 75.2µs) — single lock + bitmap acceleration
- Filtering 20k entities improved **↑4%** (76.3ms vs 79.1ms, ~262k entities/sec)
- Nav root stable at ~47µs (~21.1k req/sec)

### Federated Write Proxy — Persistent Connection to Remote

| Benchmark | Mean | Req/sec | Description |
|-----------|------|---------|-------------|
| `his_read_proxied_1000` | 3.649 ms | 274 | hisRead 1000 items, proxied through lead to remote |
| `his_write_proxied_100` | 494.8 µs | 2,021 | hisWrite 100 items, proxied through lead to remote |
| `point_write_proxied` | 120.5 µs | 8,299 | pointWrite, proxied through lead to remote |

**Observations:**
- Proxy operations reuse the persistent authenticated connection (no per-request SCRAM handshake)
- hisRead improved **↑3%** (3.65ms vs 3.77ms, ~274 req/sec)
- hisWrite improved **↑4%** (495µs vs 518µs, ~2.0k req/sec)

### Federated Concurrent Load

| Benchmark | Mean | Effective Req/sec | Description |
|-----------|------|-------------------|-------------|
| `concurrent_reads_50` | 749.4 µs | 66,720 | 50 parallel federated read-by-id requests |
| `concurrent_filter_50` | 24.0 ms | 2,083 | 50 parallel federated filter reads (`siteRef==@ra-site-N`) |

**Observations:**
- 50 concurrent federated reads improved **↑1%** (749µs vs 760µs, ~66.7k effective req/sec)
- Concurrent filter reads within noise range (~24ms vs 22ms) — concurrent contention varies run-to-run

---

## Version Comparison (0.4.x → 0.5.3 → 0.5.4)

Key improvements across versions. v0.5.4 adds query planner enhancements (auto-index, ref-bitmap, AST cache, limit-aware strategy), IndexMap LRU cache, usize-based BFS, consolidated federation CacheState, &str column discovery, and direct-buffer Zinc encoding.

| Benchmark | v0.4.x | v0.5.3 | v0.5.4 | v0.5.3→v0.5.4 |
|-----------|--------|--------|--------|----------------|
| `zinc_encode_100_rows` | 64.2 µs | 60.1 µs | 53.6 µs | **↑11%** |
| `zinc_encode_1000_rows` | 640.1 µs | 591.7 µs | 549.2 µs | **↑7%** |
| `filter_eval_simple` | 15.7 ns | 11.6 ns | 10.4 ns | **↑10%** |
| `filter_eval_complex` | 84.6 ns | 55.5 ns | 51.4 ns | **↑7%** |
| `graph_get_entity` | 22.3 ns | 18.6 ns | 16.7 ns | **↑10%** |
| `graph_update_entity` | 32.1 µs | 6.55 µs | 7.10 µs | ~(auto-index overhead) |
| `xeto_effective_slots` | 598.1 ns | 311.1 ns | 165.0 ns | **↑47%** |
| `xeto_effective_slots_inherited` | 602.3 ns | 323.4 ns | 168.8 ns | **↑48%** |
| `http_read_filter` | 820.1 µs | 501.8 µs | 459.1 µs | **↑9%** |
| `http_read_filter_large` | 4.302 ms | 3.699 ms | 3.404 ms | **↑8%** |
| `http_his_write_100` | 468.6 µs | 259.8 µs | 254.8 µs | **↑2%** |
| `fed. sync 10k` | — | 42.5 ms | 40.8 ms | **↑4%** |
| `fed. read_by_id` | 6.13 ms | 63.4 µs | 60.9 µs | **↑4%** |
| `fed. filter_site` | 13.65 ms | 75.2 µs | 72.8 µs | **↑3%** |
| `fed. filter 20k` | — | 79.1 ms | 76.3 ms | **↑4%** |
| `fed. concurrent_reads_50` | 111.7 ms | 760.1 µs | 749.4 µs | **↑1%** |

---

## Summary

| Category | Highlight | Throughput |
|----------|-----------|------------|
| Codec (Zinc encode) | 53.6µs / 100 rows | ~1.87M rows/sec |
| Codec (Zinc decode) | 105.1µs / 100 rows | ~951k rows/sec |
| Graph lookup | 16.7ns per get | ~59.9M ops/sec |
| Graph filtering | 632.3µs / 1000 entities | ~1.6k queries/sec |
| Filter evaluation | 51.4ns complex eval | ~19.5M ops/sec |
| Ontology fitting | Sub-microsecond | ~10M ops/sec |
| Xeto slot resolution | 165ns effective slots | ~6.1M ops/sec |
| HTTP read by ID | 58.6µs per request | ~17.1k req/sec |
| HTTP filter (100 results) | 459.1µs per request | ~2.2k req/sec |
| HTTP nav | 73.5µs per request | ~13.6k req/sec |
| HTTP watch poll | 48.4µs per request | ~20.7k req/sec |
| Concurrent reads (50) | 721.7µs total | ~69.3k effective req/sec |
| History read (1000 items) | 1.900ms per request | ~526 req/sec |
| Fed. sync (10k entities) | 40.8ms per remote | ~248k entities/sec |
| Fed. read by ID | 60.9µs per request | ~16.4k req/sec |
| Fed. filter (20k entities) | 76.3ms per request | ~13 req/sec |
| Fed. proxy hisRead (1000) | 3.649ms per request | ~274 req/sec |
| Fed. proxy pointWrite | 120.5µs per request | ~8.3k req/sec |
| Fed. concurrent reads (50) | 749.4µs total | ~66.7k effective req/sec |
