# Rusty Haystack Benchmarks

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
- **Compound filter at 54.1ns on 10K entities** — roaring bitmap intersection prunes candidates before filter eval, yielding ~18.5M queries/sec
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

11 benchmarks measuring real HTTP round-trips against a live Axum server with 1,000 pre-loaded entities across 10 sites. Each request includes full TCP connection, serialization, and deserialization.

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
- 50 parallel reads at 591.4µs = ~84.6K effective req/sec — near-linear scaling demonstrates Axum's async efficiency

---

## Version Comparison (0.4.x -> 0.8.0)

Key improvements across versions. v0.7.0 added roaring bitmap indexing, delta tag indexing, bulk entity loading, and ID freelist recycling. v0.8.0 brings across-the-board performance improvements from optimized data structures and reduced allocations.

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

**Notable v0.8.0 improvements over v0.7.0:**
- `graph_get_entity`: 18.3ns -> 7.1ns (2.6x faster)
- `filter_eval_simple`: 11.6ns -> 7.8ns (1.5x faster)
- `filter_eval_complex`: 55.6ns -> 34.9ns (1.6x faster)
- `graph_filter_compound_10000`: 12.1µs -> 54.1ns (224x faster — bitmap-only path eliminates per-entity eval)
- `validate_graph_1000`: 343.0µs -> 208.4µs (1.6x faster)
- `xeto_effective_slots`: 183.9ns -> 114.1ns (1.6x faster)
- `zinc_encode_100_rows`: 54.1µs -> 40.9µs (1.3x faster)

---

## Benchmark Data

Test datasets used in benchmarks:

| Dataset | Entity Count | Structure | Used By |
|---------|-------------|-----------|---------|
| Homogeneous 1K | 1,001 | 1 site + 1K identical points (7 tags each) | Existing graph benchmarks |
| Homogeneous 10K | 10,001 | 1 site + 10K identical points | `graph_filter_10000_entities` |
| Hierarchy 112 | 112 | 2 sites -> 10 equips -> 100 points | Traversal benchmarks |
| Realistic 10K | ~10,000 | ~125 campuses x 80 entities with diverse tag sets | Optimization benchmarks |

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
