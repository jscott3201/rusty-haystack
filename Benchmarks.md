# Rusty Haystack Benchmarks

## Environment

| Property | Value |
|----------|-------|
| Platform | macOS (Darwin 25.4.0, arm64) |
| CPU | Apple M2 |
| Memory | 8 GB |
| Rust | 1.93.1 |
| Version | 0.7.0 |
| Profile | release (optimized) |
| Framework | Criterion 0.8 |
| Date | 2026-03-03 |

---

## Core Operations (haystack-core)

### Codec — Encode / Decode

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `zinc_encode_scalar` | 84.9 ns | 11,779,000 | Single Number scalar |
| `zinc_decode_scalar` | 124.0 ns | 8,065,000 | Single Number scalar |
| `zinc_encode_100_rows` | 54.1 µs | 18,488 | 100-row grid, 7 columns |
| `zinc_decode_100_rows` | 105.6 µs | 9,470 | 100-row grid, 7 columns |
| `zinc_encode_1000_rows` | 541.3 µs | 1,847 | 1000-row grid, 7 columns |
| `zinc_decode_1000_rows` | 1.095 ms | 913 | 1000-row grid, 7 columns |
| `json4_encode_100_rows` | 136.2 µs | 7,342 | JSON v4, 100 rows |
| `json4_decode_100_rows` | 202.9 µs | 4,929 | JSON v4, 100 rows |
| `json4_encode_1000_rows` | 1.357 ms | 737 | JSON v4, 1000 rows |
| `json4_decode_1000_rows` | 2.174 ms | 460 | JSON v4, 1000 rows |
| `csv_encode_1000_rows` | 787.7 µs | 1,270 | CSV, 1000 rows (encode only) |
| `codec_roundtrip_mixed_types` | 422.7 µs | 2,366 | Zinc encode+decode, 100 rows, 9 mixed types |

**Observations:**
- Zinc remains the fastest text codec: ~2.5x faster encode and ~1.9x faster decode vs JSON at 100 rows
- 100-row Zinc encode at 54.1µs = ~1.85M rows/sec throughput
- CSV encode-only sits between Zinc and JSON

### HBF — Haystack Binary Format (feature: `haystack-serde`)

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `hbf_encode_100_rows` | 27.3 µs | 36,630 | Binary encode 100-row grid |
| `hbf_decode_100_rows` | 76.7 µs | 13,038 | Binary decode 100-row grid |
| `hbf_encode_1000_rows` | 235.3 µs | 4,250 | Binary encode 1000-row grid |
| `hbf_decode_1000_rows` | 755.2 µs | 1,324 | Binary decode 1000-row grid |

**Observations:**
- HBF encode is **2.0x faster** than Zinc encode (27.3µs vs 54.1µs) and **5.0x faster** than JSON encode
- HBF decode is **1.4x faster** than Zinc decode (76.7µs vs 105.6µs) and **2.6x faster** than JSON decode
- HBF 1000-row encode (235.3µs) is **2.3x faster** than Zinc (541.3µs) and **5.8x faster** than JSON (1.357ms)
- For federation sync and WebSocket watches, HBF's encode speed advantage (2-6x) outweighs the payload size trade-off

### Filter — Parse & Evaluate

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `filter_parse_simple` | 110.8 ns | 9,026,000 | Parse `site` |
| `filter_parse_complex` | 644.9 ns | 1,551,000 | Parse `site and equip and point and temp > 70°F` |
| `filter_eval_simple` | 11.6 ns | 86,207,000 | Evaluate marker check |
| `filter_eval_complex` | 55.6 ns | 17,986,000 | Evaluate 4-clause filter with comparison |

**Observations:**
- Simple filter evaluation at ~12ns = ~86.2M ops/sec
- Complex 4-clause evaluation at ~56ns = ~18.0M ops/sec
- AST caching eliminates re-parsing overhead for repeated queries

### Expression Evaluator

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `expr_parse_simple` | 173.8 ns | 5,755,000 | Parse `$x + 1` |
| `expr_parse_complex` | 871.0 ns | 1,148,000 | Parse `min($x, max($y, $z * 2.0)) + abs($a - $b)` |
| `expr_eval_simple` | 42.3 ns | 23,641,000 | Evaluate simple arithmetic |
| `expr_eval_complex` | 190.0 ns | 5,263,000 | Evaluate nested functions (min, max, abs) |

**Observations:**
- Expression parser handles complex nested-function expressions in < 1µs
- Simple arithmetic evaluation at ~42ns = ~23.6M ops/sec — suitable for real-time computed points
- Complex expression with 5 variables and 3 function calls: ~190ns = ~5.3M ops/sec

### Unit Conversion

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `unit_convert_temperature` | 92.4 ns | 10,823,000 | Convert °F → °C |
| `unit_compatible_check` | 69.6 ns | 14,368,000 | Check °F ↔ °C compatibility |
| `unit_quantity_lookup` | 28.0 ns | 35,714,000 | Look up quantity for unit |

**Observations:**
- Temperature conversion (affine transform) at ~92ns = ~10.8M ops/sec
- Compatibility check at ~70ns = ~14.4M ops/sec — fast enough for inline validation
- Quantity lookup at ~28ns = ~35.7M ops/sec — hash table lookup

### Graph — Entity Operations

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `graph_get_entity` | 18.3 ns | 54,645,000 | Get by ID from 1000-entity graph |
| `graph_add_entity` | 918.0 ns | 1,089,000 | Add single entity (with changelog, CSR patch) |
| `graph_add_1000_entities` | 2.190 ms | 457 | Add 1000 entities into fresh graph |
| `graph_update_entity` | 1.959 µs | 510,464 | Update 2 tags in 1000-entity graph (delta indexing) |
| `graph_remove_entity` | 2.527 µs | 395,726 | Remove + re-add cycle (with freelist) |
| `graph_filter_1000_entities` | 609.2 µs | 1,641 | Filter 1000 entities (`point and temp > 70°F`) |
| `graph_filter_10000_entities` | 6.632 ms | 151 | Filter 10,000 homogeneous entities (same filter) |
| `graph_changes_since` | 1.8 ns | 555,556,000 | Binary search changelog at midpoint version |
| `shared_graph_concurrent_rw` | 311.1 µs | 3,215 | 4 reader threads + 1 writer thread, 100 entities |

**Observations:**
- Entity lookup at ~18ns = ~54.6M ops/sec — single HashMap get
- **`graph_update_entity` at 1.96µs is 3.4x faster than v0.6.x** (was 6.73µs) — delta indexing only re-indexes changed tags
- `changes_since` at ~1.8ns uses binary search on VecDeque — ~556M ops/sec
- Freelist ID recycling keeps entity IDs compact after remove+re-add cycles

### Graph — Bulk & Optimization Operations (new in v0.7.0)

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `graph_bulk_add_10000` | 9.600 ms | 104 | Bulk-load 10K entities via `add_bulk`/`finalize_bulk` |
| `graph_update_delta_10000` | 2.560 µs | 390,625 | Update 2 tags on 10K realistic graph (delta path) |
| `graph_filter_realistic_10000` | 838.1 µs | 1,193 | Filter 10K diverse entities (`point and sensor and temp`) |
| `graph_filter_compound_10000` | 12.1 µs | 82,645 | Compound filter with ref equality on 10K entities |
| `graph_filter_range_10000` | 1.622 ms | 617 | Value-range filter (`curVal > 73°F`) on 10K entities |
| `graph_csr_rebuild_10000` | 1.539 ms | 650 | Full CSR adjacency rebuild for 10K entities |
| `graph_freelist_recycle_1000` | 1.374 ms | 728 | Remove + re-add 1000 entities using freelist recycling |

**Observations:**
- **Bulk add is ~2.3x faster per entity than incremental add** — skips changelog, version bumps, CSR patching, and cache resizing (9.6ms / 10K vs ~21.9ms extrapolated from `add_1000`)
- **Compound filter at 12.1µs on 10K entities** — roaring bitmap intersection prunes candidates before filter eval, yielding 82K queries/sec
- Delta update holds steady at ~2.6µs regardless of graph size (1K vs 10K) — only touches changed tags
- CSR rebuild at 1.54ms for 10K entities is fast enough for the 1000-op patch threshold
- The realistic 10K dataset uses 8 entity types (sites, AHUs, VAVs, boilers, meters, weather stations, 9 point kinds) giving diverse tag sets for bitmap filtering

### Graph — Structural Fingerprinting (new in v0.7.0)

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `structural_compute_5000` | 5.420 ms | 184 | Full WL recomputation on 5K diverse entities (depth=2) |
| `structural_fingerprint_lookup` | 12.4 ns | 80,645,000 | Look up fingerprint for a single entity |
| `structural_partitions_with_tags` | 2.916 µs | 343,000 | Find all partition bitmaps matching tag set |
| `structural_histogram` | 1.196 µs | 836,120 | Build fingerprint frequency histogram |

**Observations:**
- Full WL structural recomputation at 5.4ms for 5K entities — ~1.08µs per entity amortized
- Fingerprint lookup at ~12ns is a single HashMap get — ~80.6M ops/sec
- Partitions-with-tags query at ~2.9µs scans partition tag sets and unions matching bitmaps
- Adaptive depth: full WL refinement < 50K entities, tag-hash-only 50K-200K, skipped > 200K

### Graph — Traversal Helpers

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `graph_hierarchy_tree` | 52.4 µs | 19,084 | Build hierarchy tree from site (2 sites, 10 equips, 100 points) |
| `graph_classify` | 69.9 ns | 14,306,000 | Classify entity type from markers |
| `graph_ref_chain` | 167.2 ns | 5,981,000 | Walk point → equipRef → siteRef chain |
| `graph_children` | 3.160 µs | 316,456 | Find all children of a site |
| `graph_site_for` | 54.7 ns | 18,282,000 | Resolve site for a point |
| `graph_equip_points` | 880.5 ns | 1,135,700 | Find all points for an equip |

**Observations:**
- `site_for` at ~55ns = ~18.3M ops/sec — follows ref chain, returns in < 60ns
- `ref_chain` at ~167ns walks a 2-hop chain (point→equip→site)
- `classify` at ~70ns = ~14.3M ops/sec — determines entity type from markers
- `hierarchy_tree` builds a full 112-entity tree (2 sites × 5 equips × 10 points) in ~52µs

### Snapshot — Write / Read

| Benchmark | Mean | Description |
|-----------|------|-------------|
| `snapshot_write_1000` | 1.127 ms | Write 1001-entity graph to HLSS v2 snapshot (HBF + Zstd) |
| `snapshot_read_1000` | 2.505 ms | Read HLSS v2 snapshot and load into graph (uses `add_bulk`) |
| `snapshot_write_10000_realistic` | 16.33 ms | Write 10K diverse-entity graph to HLSS v2 snapshot |
| `snapshot_read_10000_realistic` | 37.29 ms | Read HLSS v2 snapshot and bulk-load 10K entities |

**Observations:**
- 1K snapshot write at ~1.1ms = ~887K entities/sec write throughput
- 1K snapshot read at ~2.5ms = ~399K entities/sec load throughput
- **10K realistic write at ~16.3ms = ~613K entities/sec** — 31% faster than v0.7.0 Zinc-based snapshots
- **10K realistic read at ~37.3ms = ~268K entities/sec** — 26% faster from HBF binary decode
- Pipeline: HBF binary encode → Zstd compress → CRC32 → atomic write (HLSS v2 format)
- Read path uses `add_bulk` / `finalize_bulk` — skips changelog and per-entity version bumps

### Graph Validation

| Benchmark | Mean | Description |
|-----------|------|-------------|
| `validate_graph_1000` | 343.0 µs | Validate 1001 entities against standard ontology |

**Observations:**
- Graph validation at ~343µs for 1000 entities = ~2.92M entities/sec
- Validates: spec conformance, tag types, dangling refs, spec coverage

### Ontology — Namespace Operations

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `ontology_load_standard` | 4.710 ms | 212 | Load standard namespace from bundled data |
| `ontology_fits_check` | 98.8 ns | 10,121,000 | Check if entity fits `ahu` type |
| `ontology_is_subtype` | 112.6 ns | 8,881,000 | Check `ahu` is subtype of `equip` |
| `ontology_mandatory_tags` | 76.7 ns | 13,038,000 | Get mandatory tags for `ahu` |
| `ontology_validate_entity` | 353.6 ns | 2,828,000 | Validate entity against ontology |

**Observations:**
- Namespace loading (~4.7ms) is a one-time startup cost
- All runtime ontology lookups remain sub-microsecond (~9-13M ops/sec)

### Xeto — Structural Type Fitting

| Benchmark | Mean | Ops/sec | Description |
|-----------|------|---------|-------------|
| `xeto_fits_ahu` | 409.7 ns | 2,441,000 | Fits check: entity with ahu+equip markers |
| `xeto_fits_missing_marker` | 473.8 ns | 2,111,000 | Fits check: entity missing required marker (fail path) |
| `xeto_fits_explain` | 473.6 ns | 2,111,000 | Fits with issue explanation (fail path) |
| `xeto_fits_site` | 322.8 ns | 3,098,000 | Fits check: simple site entity |
| `xeto_effective_slots` | 183.9 ns | 5,438,000 | Resolve effective slots for a spec |
| `xeto_effective_slots_inherited` | 177.8 ns | 5,624,000 | Resolve effective slots with base chain |

**Observations:**
- Effective slot resolution at ~178-184ns = ~5.4-5.6M ops/sec
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

## Version Comparison (0.4.x → 0.6.x → 0.7.x)

Key improvements across versions. v0.7.0 adds roaring bitmap indexing, delta tag indexing, incremental CSR adjacency, bulk entity loading, ID freelist recycling, and WL structural fingerprinting. v0.7.1 switches HLSS snapshots from Zinc to HBF binary codec.

| Benchmark | v0.4.x | v0.5.4 | v0.6.x | v0.7.0 | v0.7.1 | v0.7.0→v0.7.1 |
|-----------|--------|--------|--------|--------|--------|----------------|
| `zinc_encode_100_rows` | 64.2 µs | 53.6 µs | 52.1 µs | 54.1 µs | 54.1 µs | ~(noise) |
| `zinc_encode_1000_rows` | 640.1 µs | 549.2 µs | 520.1 µs | 541.3 µs | 541.3 µs | ~(noise) |
| `filter_eval_simple` | 15.7 ns | 10.4 ns | 11.7 ns | 11.6 ns | 11.6 ns | ~(noise) |
| `filter_eval_complex` | 84.6 ns | 51.4 ns | 57.7 ns | 55.6 ns | 55.6 ns | ~(noise) |
| `graph_get_entity` | 22.3 ns | 16.7 ns | 18.0 ns | 18.3 ns | 18.3 ns | ~(noise) |
| `graph_update_entity` | 32.1 µs | 7.10 µs | 6.73 µs | 1.96 µs | 1.96 µs | ~(noise) |
| `graph_filter_10000` | — | — | 7.90 ms | 6.63 ms | 6.63 ms | ~(noise) |
| `graph_changes_since` | — | 17.2 µs | 2.0 ns | 1.8 ns | 1.8 ns | ~(noise) |
| `snapshot_read_1000` | — | — | 3.19 ms | 2.81 ms | 2.50 ms | **↑11%** |
| `snapshot_write_1000` | — | — | — | 1.39 ms | 1.13 ms | **↑19%** |
| `snapshot_write_10000_realistic` | — | — | — | 22.6 ms | 16.3 ms | **↑31%** |
| `snapshot_read_10000_realistic` | — | — | — | 50.6 ms | 37.3 ms | **↑26%** |
| `validate_graph_1000` | — | — | 502.9 µs | 343.0 µs | 343.0 µs | ~(noise) |
| `xeto_effective_slots` | 598.1 ns | 165.0 ns | 320.9 ns | 183.9 ns | 183.9 ns | ~(noise) |
| `graph_bulk_add_10000` | — | — | — | 9.60 ms | 9.60 ms | ~(noise) |
| `graph_filter_compound_10000` | — | — | — | 12.1 µs | 12.1 µs | ~(noise) |
| `structural_compute_5000` | — | — | — | 5.42 ms | 5.42 ms | ~(noise) |
| `structural_fingerprint_lookup` | — | — | — | 12.4 ns | 12.4 ns | ~(noise) |

**v0.7.1 headlines:**
- **HLSS v2 snapshot format** — body codec switched from Zinc (text) to HBF (binary). Same Zstd + CRC32 envelope.
- **`snapshot_write_10000_realistic` 31% faster** — HBF binary encode is 2.2x faster than Zinc text encode, reducing the pre-compression step.
- **`snapshot_read_10000_realistic` 26% faster** — HBF binary decode avoids text parsing; combined with smaller decompressed payload.
- **`snapshot_write_1000` 19% faster** — even at smaller scale, HBF encode wins from eliminated text formatting.
- **Clean break**: HLSS v2 snapshots are not backward-compatible with v1 (Zinc). Old snapshots must be recreated.

---

## Benchmark Data

Test datasets used in benchmarks:

| Dataset | Entity Count | Structure | Used By |
|---------|-------------|-----------|---------|
| Homogeneous 1K | 1,001 | 1 site + 1K identical points (7 tags each) | Existing graph benchmarks |
| Homogeneous 10K | 10,001 | 1 site + 10K identical points | `graph_filter_10000_entities` |
| Hierarchy 112 | 112 | 2 sites → 10 equips → 100 points | Traversal benchmarks |
| Realistic 5K | ~5,000 | ~62 campuses × 80 entities (sites, AHUs, VAVs, boilers, meters, weather, 9 point kinds) | Structural benchmarks |
| Realistic 10K | ~10,000 | ~125 campuses × 80 entities with diverse tag sets | Optimization + snapshot benchmarks |

The "realistic" datasets model a multi-campus building portfolio with 8 equipment types and 9 point kinds (temp, pressure, flow, occupancy, damper, speed, setpoint, enable, alarm), providing ~25 distinct structural fingerprints for WL partitioning.

---

## Running Benchmarks

```bash
# All core benchmarks (default features)
cargo bench -p rusty-haystack-core

# Include HBF binary codec benchmarks
cargo bench -p rusty-haystack-core --features haystack-serde

# Run a specific benchmark
cargo bench -p rusty-haystack-core -- graph_filter

# Federation benchmarks (requires haystack-server)
cargo bench -p rusty-haystack-server --bench federation
```

---

## Summary

| Category | Highlight | Throughput |
|----------|-----------|------------|
| **Codec (Zinc encode)** | 54.1µs / 100 rows | ~1.85M rows/sec |
| **Codec (Zinc decode)** | 105.6µs / 100 rows | ~947K rows/sec |
| **Codec (HBF encode)** | 27.3µs / 100 rows | ~3.66M rows/sec |
| **Codec (HBF decode)** | 76.7µs / 100 rows | ~1.30M rows/sec |
| Graph lookup | 18.3ns per get | ~54.6M ops/sec |
| **Graph update (delta)** | **1.96µs per update** | **~510K ops/sec** |
| Graph add (single) | 918ns per entity | ~1.09M ops/sec |
| **Graph bulk add** | **9.6ms / 10K entities** | **~1.04M entities/sec** |
| Graph filtering (1K) | 609.2µs per query | ~1.6K queries/sec |
| **Graph compound filter (10K)** | **12.1µs per query** | **~82.6K queries/sec** |
| Graph ref_chain | 167.2ns per walk | ~6.0M ops/sec |
| Graph site_for | 54.7ns per resolve | ~18.3M ops/sec |
| Graph hierarchy_tree | 52.4µs / 112 entities | ~19.1K trees/sec |
| Graph CSR rebuild | 1.54ms / 10K entities | ~6.50M edges/sec |
| **Structural compute** | **5.42ms / 5K entities** | **~922K entities/sec** |
| **Structural lookup** | **12.4ns per fingerprint** | **~80.6M ops/sec** |
| Filter evaluation | 55.6ns complex eval | ~18.0M ops/sec |
| Expression eval | 190.0ns complex eval | ~5.3M ops/sec |
| Unit conversion | 92.4ns per convert | ~10.8M ops/sec |
| Ontology fitting | Sub-microsecond | ~10.1M ops/sec |
| Xeto slot resolution | 183.9ns effective slots | ~5.4M ops/sec |
| Snapshot write (1K) | 1.13ms / 1K entities | ~887K entities/sec |
| Snapshot read (1K) | 2.50ms / 1K entities | ~399K entities/sec |
| **Snapshot write (10K)** | **16.3ms / 10K entities** | **~613K entities/sec** |
| **Snapshot read (10K)** | **37.3ms / 10K entities** | **~268K entities/sec** |
| Graph validation | 343.0µs / 1K entities | ~2.92M entities/sec |
| HTTP read by ID | 58.6µs per request | ~17.1K req/sec |
| HTTP filter (100 results) | 459.1µs per request | ~2.2K req/sec |
| HTTP watch poll | 48.4µs per request | ~20.7K req/sec |
| Concurrent reads (50) | 721.7µs total | ~69.3K effective req/sec |
| Fed. sync (10K entities) | 39.8ms per remote | ~254K entities/sec |
| **Fed. read by ID (HBF)** | **56.5µs per request** | **~17.7K req/sec** |
| **Fed. filter (20K, HBF)** | **58.6ms per request** | **~17 req/sec** |
| Fed. proxy pointWrite | 120.5µs per request | ~8.3K req/sec |
| Fed. concurrent reads (50) | 749.4µs total | ~66.7K effective req/sec |
