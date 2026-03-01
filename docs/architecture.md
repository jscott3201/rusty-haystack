# Architecture

## Crate Dependency Graph

```
haystack-core          (foundation — no workspace deps)
    |
    +-- haystack-client    (HTTP/WS client, SCRAM handshake)
    |       |
    +-------+-- haystack-server  (HTTP API, 30+ ops, WebSocket, auth)
    |               |
    +-------+-------+-- haystack-cli   (CLI binary, depends on all three)
    |
    +-- rusty-haystack     (PyO3 Python bindings, core only)
```

`haystack-core` is the foundation. Every other crate depends on it directly. `haystack-server` also depends on `haystack-client` (for [federation](federation.md) sync). `haystack-cli` ties everything together into a single binary.

## Core Abstractions

### Kind

The `Kind` enum represents all 15 Haystack scalar types plus composite types:

```
Kind
  ├── Null, Marker, NA, Remove           (singletons)
  ├── Bool(bool)                          (boolean)
  ├── Number { val: f64, unit: Option }   (numeric with optional unit)
  ├── Str(String)                         (string)
  ├── Ref { val, dis }                    (entity reference)
  ├── Uri, Symbol, XStr                   (typed strings)
  ├── Date, Time, DateTime                (temporal, via chrono)
  ├── Coord { lat, lng }                  (geographic)
  ├── List(Vec<Kind>)                     (heterogeneous list)
  ├── Dict(Box<HDict>)                    (tag dictionary)
  └── Grid(Box<HGrid>)                    (tabular data)
```

### HDict

A mutable dictionary mapping tag names (strings) to `Kind` values. Every entity in the system is an `HDict`. Key operations:

- `has(name)` / `missing(name)` — tag presence
- `get(name)` — tag lookup
- `id()` — shortcut for the "id" Ref tag
- `set(name, val)` / `remove_tag(name)` — mutation
- `merge(other)` — merge with `Kind::Remove` support
- `sorted_iter()` — deterministic iteration order

### HGrid

A tabular data structure consisting of metadata (`HDict`), columns (`Vec<HCol>`), and rows (`Vec<HDict>`). Grids are the primary wire format for requests and responses.

- `HGrid::new()` — empty grid
- `HGrid::from_parts(meta, cols, rows)` — construct from components
- `is_err()` — check if grid represents an error (has "err" marker in meta)

### EntityGraph

An in-memory entity store with bitmap indexing for fast tag-based queries and ref adjacency for graph traversal:

```
EntityGraph
  ├── entities: HashMap<String, HDict>    (ref_val → entity)
  ├── tag_index: TagBitmapIndex           (fast has/missing queries)
  ├── adjacency: RefAdjacency             (bidirectional ref links)
  ├── namespace: Option<DefNamespace>     (ontology for spec-aware ops)
  ├── version: u64                        (monotonic counter)
  └── changelog: Vec<GraphDiff>           (capped at 10,000 entries)
```

CRUD operations: `add`, `get`, `update`, `remove`. Query operations: `read(filter, limit)`.

### SharedGraph

Thread-safe wrapper: `Arc<RwLock<EntityGraph>>` using `parking_lot::RwLock`. Provides `read(closure)` and `write(closure)` for lock-scoped access, plus convenience methods for common operations.

## Codec Pipeline

All codecs implement the `Codec` trait:

```rust
pub trait Codec: Send + Sync {
    fn mime_type(&self) -> &str;
    fn encode_grid(&self, grid: &HGrid) -> Result<String, CodecError>;
    fn decode_grid(&self, input: &str) -> Result<HGrid, CodecError>;
    fn encode_scalar(&self, val: &Kind) -> Result<String, CodecError>;
    fn decode_scalar(&self, input: &str) -> Result<Kind, CodecError>;
}
```

Codecs are registered in a static registry accessed via `codecs::codec_for(mime)`:

| MIME Type | Codec | Notes |
|-----------|-------|-------|
| `text/zinc` | ZincCodec | Primary format, ~2x faster than JSON |
| `text/trio` | TrioCodec | Record-per-entity format |
| `application/json` | Json4Codec | JSON Haystack v4 (`_kind` discriminator) |
| `application/json;v=3` | Json3Codec | JSON Haystack v3 (type-prefix strings) |
| `text/csv` | CsvCodec | Encode-only |

## Auth Flow (SCRAM SHA-256)

The server implements [RFC 5802](https://tools.ietf.org/html/rfc5802) SCRAM-SHA-256 authentication:

```
Client                              Server
  |                                    |
  |-- GET /api/about ----------------->|
  |   Authorization: HELLO             |
  |   username=<base64(user)>          |
  |                                    |
  |<--- 401 Unauthorized --------------|
  |   WWW-Authenticate: SCRAM          |
  |   handshakeToken=<tok>             |
  |   hash=SHA-256                     |
  |   data=<server_first_b64>          |
  |                                    |
  |-- GET /api/about ----------------->|
  |   Authorization: SCRAM             |
  |   handshakeToken=<tok>             |
  |   data=<client_final_b64>          |
  |                                    |
  |<--- 200 OK ------------------------|
  |   Authentication-Info:             |
  |   authToken=<tok>                  |
  |   data=<server_final_b64>          |
  |                                    |
  |-- POST /api/read ----------------->|
  |   Authorization: BEARER            |
  |   authToken=<tok>                  |
  |                                    |
  |<--- 200 OK ------------------------|
```

Security measures:
- 100,000 PBKDF2 iterations
- Constant-time credential comparison (`subtle` crate)
- Fake challenge for unknown users (prevents enumeration)
- 60s handshake TTL, configurable token TTL (default 3600s)

## Server Request Lifecycle

1. **TCP accept** — Actix Web receives the connection
2. **Payload parsing** — body read up to 2 MB limit
3. **Auth middleware** — checks Authorization header:
   - `/api/about`, `/api/ops`, `/api/formats` pass through
   - All others require BEARER token (if auth is enabled)
   - Permission check: read / write / admin based on endpoint
4. **Content negotiation** — `Content-Type` header selects request codec, `Accept` header selects response codec (default: `text/zinc`)
5. **Op handler** — decodes request grid, executes operation, encodes response grid
6. **Response** — grid serialized with negotiated codec and returned

## Ontology

The `DefNamespace` loads bundled Haystack 4 definitions (`ph`, `phScience`, `phIoT`, `phIct`) and optional Xeto specs. It provides:

- **Taxonomy** — `is_a(name, supertype)` for nominal subtype checking
- **Fitting** — `fits(entity, type_name)` for structural type matching
- **Validation** — `validate_entity(entity)` for ontology conformance
- **Xeto management** — `load_xeto(source, lib)`, `unload_lib(name)`, `get_spec(qname)`

## Filter Engine

Filter expressions are parsed into an AST (`FilterNode`) by a hand-written recursive descent parser, then evaluated against entities:

```
FilterNode
  ├── Has(path)                    tag exists
  ├── Missing(path)                tag missing
  ├── Cmp { path, op, val }       comparison (==, !=, <, <=, >, >=)
  ├── And(left, right)             logical and
  ├── Or(left, right)              logical or
  └── SpecMatch(type_name)         ontology type match (e.g., "ph::Ahu")
```

Paths support ref traversal (e.g., `equipRef->siteRef->area`) via a resolver callback. Evaluation short-circuits on And/Or.
