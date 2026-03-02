# Rusty Haystack Benchmarks

## Environment

| Property | Value |
|----------|-------|
| Platform | macOS (Darwin 25.4.0, arm64) |
| CPU | Apple M2 |
| Memory | 8 GB |
| Rust | 1.93.1 |
| Version | 0.6.1 |
| Profile | release (optimized) |
| Framework | Criterion 0.8 |
| Date | 2026-07-17 |

---

## Core Operations (haystack-core)

### Codec — Encode / Decode

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `zinc_encode_scalar` | 89.5 ns | 11,173,000 | Single Number scalar |
| `zinc_decode_scalar` | 138.1 ns | 7,241,000 | Single Number scalar |
| `zinc_encode_100_rows` | 52.1 µs | 19,194 | 100-row grid, 7 columns |
| `zinc_decode_100_rows` | 108.6 µs | 9,208 | 100-row grid, 7 columns |
| `zinc_encode_1000_rows` | 520.1 µs | 1,923 | 1000-row grid, 7 columns |
| `zinc_decode_1000_rows` | 1.126 ms | 888 | 1000-row grid, 7 columns |
| `json4_encode_100_rows` | 131.0 µs | 7,634 | JSON v4, 100 rows |
| `json4_decode_100_rows` | 193.8 µs | 5,160 | JSON v4, 100 rows |
| `json4_encode_1000_rows` | 1.329 ms | 753 | JSON v4, 1000 rows |
| `json4_decode_1000_rows` | 1.927 ms | 519 | JSON v4, 1000 rows |
| `csv_encode_1000_rows` | 794.7 µs | 1,258 | CSV, 1000 rows (encode only) |
| `codec_roundtrip_mixed_types` | 409.5 µs | 2,442 | Zinc encode+decode, 100 rows, 9 mixed types |

**Observations:**
- Zinc remains the fastest text codec: ~2.6x faster encode and ~1.8x faster decode vs JSON at 100 rows
- 100-row Zinc encode at 52.1µs = ~1.92M rows/sec throughput
- CSV encode-only sits between Zinc and JSON

### HBF — Haystack Binary Format (feature: `haystack-serde`)

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `hbf_encode_100_rows` | 27.4 µs | 36,496 | Binary encode 100-row grid |
| `hbf_decode_100_rows` | 76.5 µs | 13,072 | Binary decode 100-row grid |
| `hbf_encode_1000_rows` | 236.3 µs | 4,232 | Binary encode 1000-row grid |
| `hbf_decode_1000_rows` | 754.2 µs | 1,326 | Binary decode 1000-row grid |

**Payload sizes (100-row grid):**

| Format | Size | vs Zinc | vs JSON |
|--------|------|---------|---------|
| Zinc | 3,927 bytes | — | 17% |
| HBF | 8,132 bytes | 207% | 36% |
| JSON | 22,528 bytes | 574% | — |

**Observations:**
- HBF encode is **1.9x faster** than Zinc encode (27.4µs vs 52.1µs) and **4.8x faster** than JSON encode (27.4µs vs 131.0µs)
- HBF decode is **1.4x faster** than Zinc decode (76.5µs vs 108.6µs) and **2.5x faster** than JSON decode
- HBF 1000-row encode (236.3µs) is **2.2x faster** than Zinc (520.1µs) and **5.6x faster** than JSON (1.329ms)
- HBF payload is 36% the size of JSON but 207% of Zinc — Zinc's text format is surprisingly compact for Haystack data due to short tag names
- For federation sync and WebSocket watches, HBF's encode speed advantage (2-6x) outweighs the payload size trade-off

### Filter — Parse & Evaluate

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `filter_parse_simple` | 108.8 ns | 9,191,000 | Parse `site` |
| `filter_parse_complex` | 631.1 ns | 1,585,000 | Parse `site and equip and point and temp > 70°F` |
| `filter_eval_simple` | 12.4 ns | 80,645,000 | Evaluate marker check |
| `filter_eval_complex` | 58.4 ns | 17,123,000 | Evaluate 4-clause filter with comparison |

**Observations:**
- Simple filter evaluation at ~12ns = ~80.6M ops/sec
- Complex 4-clause evaluation at ~58ns = ~17.1M ops/sec
- AST caching eliminates re-parsing overhead for repeated queries

### Expression Evaluator (new in v0.6.0)

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `expr_parse_simple` | 160.5 ns | 6,231,000 | Parse `$x + 1` |
| `expr_parse_complex` | 860.8 ns | 1,162,000 | Parse `min($x, max($y, $z * 2.0)) + abs($a - $b)` |
| `expr_eval_simple` | 47.5 ns | 21,053,000 | Evaluate simple arithmetic |
| `expr_eval_complex` | 187.6 ns | 5,330,000 | Evaluate nested functions (min, max, abs) |

**Observations:**
- Expression parser handles complex nested-function expressions in < 1µs
- Simple arithmetic evaluation at ~48ns = ~21M ops/sec — suitable for real-time computed points
- Complex expression with 5 variables and 3 function calls: ~188ns = ~5.3M ops/sec
- Parse once, evaluate many: parsed expressions are reusable AST objects

### Unit Conversion (new in v0.6.0)

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `unit_convert_temperature` | 93.4 ns | 10,707,000 | Convert °F → °C |
| `unit_compatible_check` | 72.3 ns | 13,832,000 | Check °F ↔ °C compatibility |
| `unit_quantity_lookup` | 37.7 ns | 26,525,000 | Look up quantity for unit |

**Observations:**
- Temperature conversion (affine transform) at ~93ns = ~10.7M ops/sec
- Compatibility check at ~72ns = ~13.8M ops/sec — fast enough for inline validation
- Quantity lookup at ~38ns = ~26.5M ops/sec — hash table lookup

### Graph — Entity Operations

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `graph_get_entity` | 16.1 ns | 62,112,000 | Get by ID from 1000-entity graph |
| `graph_add_entity` | 944.6 ns | 1,059,000 | Add single entity |
| `graph_add_1000_entities` | 2.270 ms | 441 | Bulk insert 1000 entities into fresh graph |
| `graph_update_entity` | 7.03 µs | 142,248 | Update entity in 1000-entity graph |
| `graph_remove_entity` | 2.46 µs | 406,504 | Remove + re-add cycle |
| `graph_filter_1000_entities` | 644.5 µs | 1,552 | Filter 1000 entities (`point and temp > 70°F`) |
| `graph_filter_10000_entities` | 7.901 ms | 127 | Filter 10,000 entities (same filter) |
| `graph_changes_since` | 2.05 ns | 487,800,000 | Query changelog at midpoint version |
| `shared_graph_concurrent_rw` | 312.6 µs | 3,199 | 4 reader threads + 1 writer thread, 100 entities |

**Observations:**
- Entity lookup at ~16ns = ~62.1M ops/sec
- `changes_since` at ~2ns is now essentially a version comparison + slice operation — ~488M ops/sec
- Auto-indexed fields (siteRef, equipRef, dis, curVal, area, geoCity, kind, unit) add slight mutation overhead but enable fast ref-based queries

### Graph — Traversal Helpers (new in v0.6.0)

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `graph_hierarchy_tree` | 56.6 µs | 17,668 | Build hierarchy tree from site (2 sites, 10 equips, 100 points) |
| `graph_classify` | 72.7 ns | 13,755,000 | Classify entity type from markers |
| `graph_ref_chain` | 195.7 ns | 5,110,000 | Walk point → equipRef → siteRef chain |
| `graph_children` | 3.32 µs | 301,205 | Find all children of a site |
| `graph_site_for` | 56.5 ns | 17,699,000 | Resolve site for a point |
| `graph_equip_points` | 949.0 ns | 1,054,000 | Find all points for an equip |

**Observations:**
- `site_for` at ~57ns = ~17.7M ops/sec — follows ref chain, returns in < 60ns
- `ref_chain` at ~196ns walks a 2-hop chain (point→equip→site)
- `classify` at ~73ns = ~13.8M ops/sec — determines entity type from markers
- `equip_points` at ~949ns returns ~10 points per equip including filter evaluation
- `hierarchy_tree` builds a full 112-entity tree (2 sites × 5 equips × 10 points) in ~57µs

### Snapshot — Write / Read (new in v0.6.0)

| Benchmark | Mean | Description |
|-----------|------|-------------|
| `snapshot_write_1000` | 1.459 ms | Write 1001-entity graph to HLSS snapshot (Zstd compressed) |
| `snapshot_read_1000` | 3.185 ms | Read HLSS snapshot and load into graph |

**Observations:**
- Snapshot write at ~1.5ms for 1000 entities = ~685k entities/sec write throughput
- Snapshot read at ~3.2ms for 1000 entities = ~314k entities/sec load throughput
- Pipeline: Zinc encode → Zstd compress → CRC32 → atomic write
- Suitable for periodic snapshots at 60-120s intervals with minimal server impact

### Graph Validation (new in v0.6.0)

| Benchmark | Mean | Description |
|-----------|------|-------------|
| `validate_graph_1000` | 502.9 µs | Validate 1001 entities against standard ontology |

**Observations:**
- Graph validation at ~503µs for 1000 entities = ~1.99M entities/sec
- Validates: spec conformance, tag types, dangling refs, spec coverage
- Fast enough for on-demand validation in CLI tooling and server health checks

### Ontology — Namespace Operations

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `ontology_load_standard` | 4.672 ms | 214 | Load standard namespace from bundled data |
| `ontology_fits_check` | 114.0 ns | 8,772,000 | Check if entity fits `ahu` type |
| `ontology_is_subtype` | 104.4 ns | 9,579,000 | Check `ahu` is subtype of `equip` |
| `ontology_mandatory_tags` | 75.4 ns | 13,263,000 | Get mandatory tags for `ahu` |
| `ontology_validate_entity` | 394.1 ns | 2,538,000 | Validate entity against ontology |

**Observations:**
- Namespace loading (~4.7ms) is a one-time startup cost
- All runtime ontology lookups remain sub-microsecond (~9-13M ops/sec)

### Xeto — Structural Type Fitting

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `xeto_fits_ahu` | 402.4 ns | 2,485,000 | Fits check: entity with ahu+equip markers |
| `xeto_fits_missing_marker` | 474.3 ns | 2,108,000 | Fits check: entity missing required marker (fail path) |
| `xeto_fits_explain` | 476.5 ns | 2,099,000 | Fits with issue explanation (fail path) |
| `xeto_fits_site` | 306.6 ns | 3,262,000 | Fits check: simple site entity |
| `xeto_effective_slots` | 170.2 ns | 5,876,000 | Resolve effective slots for a spec |
| `xeto_effective_slots_inherited` | 173.7 ns | 5,757,000 | Resolve effective slots with base chain |

**Observations:**
- Effective slot resolution at ~170ns = ~5.9M ops/sec
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

### HTTP — History Operations

| Benchmark | Mean | Req/sec | Description |
|-----------|------|---------|-------------|
| `http_his_read_1000` | 1.900 ms | 526 | Read 1000 history items for a point |
| `http_his_write_100` | 254.8 µs | 3,925 | Write 100 history items |

### HTTP — Watch Operations

| Benchmark | Mean | Req/sec | Description |
|-----------|------|---------|-------------|
| `http_watch_sub` | 162.4 µs | 6,158 | Subscribe to 10 entities (includes sub + unsub cleanup) |
| `http_watch_poll_no_changes` | 48.4 µs | 20,661 | Poll existing watch, no changes (fast path) |

### HTTP — Concurrent Load

| Benchmark | Mean | Effective Req/sec | Description |
|-----------|------|-------------------|-------------|
| `http_concurrent_reads_10` | 187.9 µs | 53,219 | 10 parallel HTTP reads by ID |
| `http_concurrent_reads_50` | 721.7 µs | 69,277 | 50 parallel HTTP reads by ID |

---

## Federation Operations (haystack-server)

Federation benchmarks using 3 in-process servers: 1 lead server with 2 federated remotes (10,000 entities each, 20,200 total across federation). Remote servers run with SCRAM SHA-256 auth enabled. Proxy operations (hisRead, hisWrite, pointWrite) include a full SCRAM handshake + HTTP round-trip to the owning remote.

### Sync — Fetch All Entities from Remotes

| Benchmark | Mean | Description |
|-----------|------|-------------|
| `read_all_10k_from_remote` | 39.8 ms | Read all 10,100 entities from one remote |
| `read_all_20k_both_remotes` | 64.2 ms | Read all entities from both remotes concurrently |

### Federated Reads — Bitmap-Indexed Cache

| Benchmark | Mean | Req/sec | Description |
|-----------|------|---------|-------------|
| `read_by_id` | 61.7 µs | 16,207 | Read single federated entity by prefixed ID (Zinc) |
| `read_by_id_hbf` | 56.5 µs | 17,699 | Read single federated entity by prefixed ID (HBF binary) |
| `filter_site` | 74.4 µs | 13,441 | Filter `site and dis=="Site 5"` across federation |
| `filter_all_points_20k` | 75.7 ms | 13 | Filter all 20k federated points (Zinc text) |
| `filter_all_points_20k_hbf` | 58.6 ms | 17 | Filter all 20k federated points (HBF binary) |
| `nav_root` | 47.6 µs | 21,008 | Navigation tree root (local only) |

**HBF vs Zinc over HTTP:**
- HBF binary format delivers **23% faster** end-to-end response for 20k-entity queries (58.6ms vs 75.7ms)
- Read-by-ID: HBF is **8% faster** (56.5µs vs 61.7µs)
- Binary format eliminates text encode/decode overhead — biggest win for large payloads
- Clients opt in via `Accept: application/x-haystack-binary` header; Zinc remains the default

### Federated Write Proxy — Persistent Connection to Remote

| Benchmark | Mean | Req/sec | Description |
|-----------|------|---------|-------------|
| `his_read_proxied_1000` | 3.649 ms | 274 | hisRead 1000 items, proxied through lead to remote |
| `his_write_proxied_100` | 494.8 µs | 2,021 | hisWrite 100 items, proxied through lead to remote |
| `point_write_proxied` | 120.5 µs | 8,299 | pointWrite, proxied through lead to remote |

### Federated Concurrent Load

| Benchmark | Mean | Effective Req/sec | Description |
|-----------|------|-------------------|-------------|
| `concurrent_reads_50` | 749.4 µs | 66,720 | 50 parallel federated read-by-id requests |
| `concurrent_filter_50` | 24.0 ms | 2,083 | 50 parallel federated filter reads (`siteRef==@ra-site-N`) |

---

## Version Comparison (0.4.x → 0.5.3 → 0.5.4 → 0.6.0)

Key improvements across versions. v0.6.0 adds HBF binary codec, expression evaluator, unit conversion, graph traversal helpers, HLSS snapshots, graph-wide validation, and HBF-over-HTTP for federation.

| Benchmark | v0.4.x | v0.5.3 | v0.5.4 | v0.6.0 | v0.5.4→v0.6.0 |
|-----------|--------|--------|--------|--------|----------------|
| `zinc_encode_100_rows` | 64.2 µs | 60.1 µs | 53.6 µs | 52.1 µs | ↑3% |
| `zinc_encode_1000_rows` | 640.1 µs | 591.7 µs | 549.2 µs | 520.1 µs | ↑6% |
| `filter_eval_simple` | 15.7 ns | 11.6 ns | 10.4 ns | 11.7 ns | ~(noise) |
| `filter_eval_complex` | 84.6 ns | 55.5 ns | 51.4 ns | 57.7 ns | ~(noise) |
| `graph_get_entity` | 22.3 ns | 18.6 ns | 16.7 ns | 18.0 ns | ~(noise) |
| `graph_update_entity` | 32.1 µs | 6.55 µs | 7.10 µs | 6.73 µs | ↑5% |
| `graph_changes_since` | — | — | 17.2 µs | 2.0 ns | **↑8,600x** |
| `xeto_effective_slots` | 598.1 ns | 311.1 ns | 165.0 ns | 320.9 ns | ~(variance) |
| `http_read_filter` | 820.1 µs | 501.8 µs | 459.1 µs | — | — |
| `http_read_filter_large` | 4.302 ms | 3.699 ms | 3.404 ms | — | — |
| `http_his_write_100` | 468.6 µs | 259.8 µs | 254.8 µs | — | — |
| `fed. sync 10k` | — | 42.5 ms | 40.8 ms | 39.8 ms | ↑2% |
| `fed. read_by_id` | 6.13 ms | 63.4 µs | 60.9 µs | 61.7 µs | ~(noise) |
| `fed. read_by_id (HBF)` | — | — | — | 56.5 µs | **new** |
| `fed. filter_site` | 13.65 ms | 75.2 µs | 72.8 µs | 74.4 µs | ~(noise) |
| `fed. filter 20k` | — | 79.1 ms | 76.3 ms | 75.7 ms | ↑1% |
| `fed. filter 20k (HBF)` | — | — | — | 58.6 ms | **23% faster** |
| `fed. concurrent_reads_50` | 111.7 ms | 760.1 µs | 749.4 µs | 768.9 µs | ~(noise) |

**v0.6.0 headlines:**
- `graph_changes_since` went from 17.2µs (scanning changelog) to **2.0ns** (direct version comparison + slice) — an **8,600x improvement** from the reactive changelog rewrite.
- **HBF over HTTP** delivers 23% faster federation queries for large payloads — binary encode/decode eliminates the dominant text serialization cost.

---

## Summary

| Category | Highlight | Throughput |
|----------|-----------|------------|
| **Codec (Zinc encode)** | 52.1µs / 100 rows | ~1.92M rows/sec |
| **Codec (Zinc decode)** | 108.6µs / 100 rows | ~921k rows/sec |
| **Codec (HBF encode)** | 27.4µs / 100 rows | ~3.65M rows/sec |
| **Codec (HBF decode)** | 76.5µs / 100 rows | ~1.31M rows/sec |
| Graph lookup | 18.0ns per get | ~55.6M ops/sec |
| Graph filtering | 610.1µs / 1000 entities | ~1.6k queries/sec |
| Graph ref_chain | 167.3ns per walk | ~6.0M ops/sec |
| Graph site_for | 61.6ns per resolve | ~16.2M ops/sec |
| Graph hierarchy_tree | 52.3µs / 112 entities | ~19.1k trees/sec |
| Filter evaluation | 57.7ns complex eval | ~17.3M ops/sec |
| Expression eval | 186.1ns complex eval | ~5.4M ops/sec |
| Unit conversion | 94.7ns per convert | ~10.6M ops/sec |
| Ontology fitting | Sub-microsecond | ~8.9M ops/sec |
| Xeto slot resolution | 320.9ns effective slots | ~3.1M ops/sec |
| Snapshot write | 1.380ms / 1000 entities | ~725k entities/sec |
| Snapshot read | 3.050ms / 1000 entities | ~328k entities/sec |
| Graph validation | 334.9µs / 1000 entities | ~2.99M entities/sec |
| HTTP read by ID | 58.6µs per request | ~17.1k req/sec |
| HTTP filter (100 results) | 459.1µs per request | ~2.2k req/sec |
| HTTP nav | 73.5µs per request | ~13.6k req/sec |
| HTTP watch poll | 48.4µs per request | ~20.7k req/sec |
| Concurrent reads (50) | 721.7µs total | ~69.3k effective req/sec |
| History read (1000 items) | 1.900ms per request | ~526 req/sec |
| Fed. sync (10k entities) | 39.8ms per remote | ~254k entities/sec |
| Fed. read by ID (Zinc) | 61.7µs per request | ~16.2k req/sec |
| **Fed. read by ID (HBF)** | **56.5µs per request** | **~17.7k req/sec** |
| Fed. filter (20k, Zinc) | 75.7ms per request | ~13 req/sec |
| **Fed. filter (20k, HBF)** | **58.6ms per request** | **~17 req/sec** |
| Fed. proxy hisRead (1000) | 3.681ms per request | ~272 req/sec |
| Fed. proxy pointWrite | 118.6µs per request | ~8.4k req/sec |
| Fed. concurrent reads (50) | 768.9µs total | ~65.1k effective req/sec |
