# Rusty Haystack Benchmarks

## Environment

| Property | Value |
|----------|-------|
| Platform | macOS (Darwin 25.4.0, arm64) |
| CPU | Apple M2 |
| Memory | 8 GB |
| Rust | 1.87.0 (17067e9ac 2025-05-09) |
| Profile | release (optimized) |
| Framework | Criterion 0.5 |
| Date | 2026-02-28 |

---

## Core Operations (haystack-core)

### Codec — Encode / Decode

| Benchmark | Mean | Description |
|-----------|------|-------------|
| `zinc_encode_scalar` | 111 ns | Single Number scalar |
| `zinc_decode_scalar` | 144 ns | Single Number scalar |
| `zinc_encode_100_rows` | 74.2 us | 100-row grid, 7 columns |
| `zinc_decode_100_rows` | 128.4 us | 100-row grid, 7 columns |
| `zinc_encode_1000_rows` | 649.6 us | 1000-row grid, 7 columns |
| `zinc_decode_1000_rows` | 1.067 ms | 1000-row grid, 7 columns |
| `json4_encode_100_rows` | 146.8 us | JSON v4, 100 rows |
| `json4_decode_100_rows` | 216.1 us | JSON v4, 100 rows |
| `json4_encode_1000_rows` | 1.399 ms | JSON v4, 1000 rows |
| `json4_decode_1000_rows` | 1.953 ms | JSON v4, 1000 rows |
| `csv_encode_1000_rows` | 788.2 us | CSV, 1000 rows (encode only) |
| `codec_roundtrip_mixed_types` | 419.3 us | Zinc encode+decode, 100 rows, 9 mixed types |

**Observations:**
- Zinc is ~2x faster than JSON for both encoding and decoding
- Encoding scales linearly: 1000 rows = ~8.7x the time of 100 rows (zinc encode)
- Mixed-type roundtrip (9 types including DateTime, Uri, Bool) adds ~4.5x overhead vs homogeneous 100-row encode

### Filter — Parse & Evaluate

| Benchmark | Mean | Description |
|-----------|------|-------------|
| `filter_parse_simple` | 87.7 ns | Parse `site` |
| `filter_parse_complex` | 576.4 ns | Parse `site and equip and point and temp > 70°F` |
| `filter_eval_simple` | 16.7 ns | Evaluate marker check |
| `filter_eval_complex` | 91.3 ns | Evaluate 4-clause filter with comparison |

**Observations:**
- Filter evaluation is extremely fast (sub-100ns even for complex filters)
- Parsing is ~6.5x slower for complex multi-clause filters vs simple tag checks

### Graph — Entity Operations

| Benchmark | Mean | Description |
|-----------|------|-------------|
| `graph_get_entity` | 17.0 ns | Get by ID from 1000-entity graph |
| `graph_add_entity` | 669.2 ns | Add single entity |
| `graph_add_1000_entities` | 1.874 ms | Bulk insert 1000 entities into fresh graph |
| `graph_update_entity` | 32.0 us | Update entity in 1000-entity graph |
| `graph_remove_entity` | 1.77 us | Remove + re-add cycle |
| `graph_filter_1000_entities` | 723.4 us | Filter 1000 entities (`point and temp > 70°F`) |
| `graph_filter_10000_entities` | 8.057 ms | Filter 10,000 entities (same filter) |
| `graph_changes_since` | 14.9 ns | Query changelog at midpoint version |
| `shared_graph_concurrent_rw` | 272.7 us | 4 reader threads + 1 writer thread, 100 entities |

**Observations:**
- Entity lookup is O(1) at ~17ns via HashMap
- Graph filtering scales linearly: 10k entities = ~11.1x the time of 1k entities
- Bulk insert of 1000 entities averages ~1.87us per entity
- Changelog query is near-instant at ~15ns
- Concurrent SharedGraph contention (4 readers + 1 writer, 10 ops each) takes ~273us total

### Ontology — Namespace Operations

| Benchmark | Mean | Description |
|-----------|------|-------------|
| `ontology_load_standard` | 4.672 ms | Load standard namespace from bundled data |
| `ontology_fits_check` | 102.7 ns | Check if entity fits `ahu` type |
| `ontology_is_subtype` | 106.4 ns | Check `ahu` is subtype of `equip` |
| `ontology_mandatory_tags` | 71.8 ns | Get mandatory tags for `ahu` |
| `ontology_validate_entity` | 341.2 ns | Validate entity against ontology |

**Observations:**
- Namespace loading (~4.7ms) is a one-time startup cost
- All runtime ontology lookups are sub-microsecond

### Xeto — Structural Type Fitting

| Benchmark | Mean | Description |
|-----------|------|-------------|
| `xeto_fits_ahu` | 432.2 ns | Fits check: entity with ahu+equip markers |
| `xeto_fits_missing_marker` | 522.7 ns | Fits check: entity missing required marker (fail path) |
| `xeto_fits_explain` | 495.1 ns | Fits with issue explanation (fail path) |
| `xeto_fits_site` | 322.3 ns | Fits check: simple site entity |
| `xeto_effective_slots` | 357.6 ns | Resolve effective slots for a spec |
| `xeto_effective_slots_inherited` | 359.2 ns | Resolve effective slots with base chain |

**Observations:**
- Xeto fitting is sub-microsecond for all cases
- Simpler specs (site) are ~25% faster than complex ones (ahu)
- Fail-path (missing marker) takes only ~20% longer than success path
- Slot resolution with inheritance has negligible overhead vs direct slots

---

## Server Operations (haystack-server)

Real HTTP benchmarks against a live server (actix-web) with 1000 pre-loaded entities across 10 sites. Each request includes full HTTP round-trip (TCP, serialize, deserialize).

### HTTP — Standard Operations

| Benchmark | Mean | Description |
|-----------|------|-------------|
| `http_about` | 53.9 us | Server info endpoint |
| `http_read_by_id` | 57.1 us | Read single entity by ID |
| `http_read_filter` | 622.2 us | Filter returning ~100 entities (`siteRef==@site-0`) |
| `http_read_filter_large` | 3.672 ms | Filter returning all 1000 entities |
| `http_nav` | 70.3 us | Navigation tree root |

**Observations:**
- Baseline HTTP overhead is ~54us (about op)
- Single entity read adds ~3us over baseline
- Filtering ~100 entities: ~622us (server-side filtering + serialization)
- Large result set (1000 entities) dominated by serialization at ~3.7ms

### HTTP — History Operations

| Benchmark | Mean | Description |
|-----------|------|-------------|
| `http_his_read_1000` | 1.947 ms | Read 1000 history items for a point |
| `http_his_write_100` | 252.3 us | Write 100 history items |

**Observations:**
- History read of 1000 items takes ~1.95ms (includes serialization of timestamps + values)
- History write of 100 items takes ~252us (~2.5us per item)

### HTTP — Watch Operations

| Benchmark | Mean | Description |
|-----------|------|-------------|
| `http_watch_sub` | 155.2 us | Subscribe to 10 entities (includes sub + unsub cleanup) |
| `http_watch_poll_no_changes` | 47.3 us | Poll existing watch, no changes (fast path) |

**Observations:**
- Watch subscribe + unsubscribe cycle takes ~155us
- Watch poll with no changes is faster than about (~47us vs ~54us) — minimal processing

### HTTP — Concurrent Load

| Benchmark | Mean | Description |
|-----------|------|-------------|
| `http_concurrent_reads_10` | 192.5 us | 10 parallel HTTP reads by ID |
| `http_concurrent_reads_50` | 736.5 us | 50 parallel HTTP reads by ID |

**Observations:**
- 10 concurrent reads complete in ~193us total (~19us effective per request vs 57us sequential)
- 50 concurrent reads complete in ~737us total (~15us effective per request)
- Near-linear scaling: 50 clients = ~3.8x the time of 10 clients (5x the load)
- The server handles concurrent reads efficiently with minimal contention

---

## Summary

| Category | Highlight |
|----------|-----------|
| Codec throughput | Zinc: ~1,540 rows/ms encode, ~937 rows/ms decode |
| Graph lookup | 17ns per entity get (O(1) HashMap) |
| Graph filtering | ~723us for 1000 entities with complex filter |
| Ontology fitting | Sub-microsecond for all type checks |
| HTTP baseline | ~54us per request (about op) |
| HTTP filter (100 results) | ~622us end-to-end |
| Concurrent HTTP (50 clients) | ~737us total, ~15us effective per request |
| History read (1000 items) | ~1.95ms end-to-end |
