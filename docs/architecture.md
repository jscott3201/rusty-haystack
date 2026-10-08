# Architecture

## Crate Dependency Graph

| Crate | Workspace dependencies | Responsibility |
| --- | --- | --- |
| `haystack-core` | None | Types, codecs, pure graph and query/catalog machinery |
| `haystack-app` | core | Authorized, bounded application reads shared by embedding and HTTP |
| `haystack-client` | core | HTTP/WebSocket client and authentication |
| `haystack-server` | core, app, client | HTTP profiles, authentication and legacy providers |
| `haystack-cli` | core, client, server | Command-line application |
| `rusty-haystack` | core, client, server | Python bindings |

The application read service has no HTTP-framework dependency. Its scoped HTTP
adapter and embedded callers share the same policy and execution contracts;
see [shared reads](shared-reads.md). The legacy server profile remains explicit
compatibility behavior.

## Core Abstractions

### Kind

The `Kind` enum represents all 15 Haystack scalar types plus composite types:

```
Kind
  +-- Null, Marker, NA, Remove           (singletons)
  +-- Bool(bool)                          (boolean)
  +-- Number { val: f64, unit: Option }   (numeric with optional unit)
  +-- Str(String)                         (string)
  +-- Ref { val, dis }                    (entity reference)
  +-- Uri, Symbol, XStr                   (typed strings)
  +-- Date, Time, DateTime                (temporal, via chrono)
  +-- Coord { lat, lng }                  (geographic)
  +-- List(Vec<Kind>)                     (heterogeneous list)
  +-- Dict(Box<HDict>)                    (tag dictionary)
  +-- Grid(Box<HGrid>)                    (tabular data)
```

### HDict

A mutable dictionary mapping tag names (strings) to `Kind` values. Every entity in the system is an `HDict`. Key operations:

- `has(name)` / `missing(name)` -- tag presence
- `get(name)` -- tag lookup
- `id()` -- shortcut for the "id" Ref tag
- `set(name, val)` / `remove_tag(name)` -- mutation
- `merge(other)` -- merge with `Kind::Remove` support
- `sorted_iter()` -- deterministic iteration order

### HGrid

A tabular data structure consisting of metadata (`HDict`), columns (`Vec<HCol>`), and rows (`Vec<HDict>`). Grids are the primary wire format for requests and responses.

- `HGrid::new()` -- empty grid
- `HGrid::from_parts(meta, cols, rows)` -- construct from components
- `is_err()` -- check if grid represents an error (has "err" marker in meta)

### EntityGraph

An in-memory entity store with bitmap indexing for fast tag-based queries and ref adjacency for graph traversal:

```
EntityGraph
  +-- entities: BTreeMap<String, HDict>    (ref_val -> entity)
  +-- tag_index: TagBitmapIndex           (fast has/missing queries)
  +-- adjacency: RefAdjacency             (bidirectional ref links)
  +-- namespace: Option<Arc<DefNamespace>>     (ontology for spec-aware ops)
  +-- version: u64                        (entity change counter)
  +-- catalog_generation: u64             (namespace publication counter)
  +-- changelog: Vec<GraphDiff>           (capped at 50,000 entries)
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

The HTTP profile follows [Haystack Auth](https://project-haystack.org/doc/docHaystack/Auth), using SCRAM-SHA-256 from [RFC 7677](https://www.rfc-editor.org/rfc/rfc7677.html). All three authentication requests use `/api/about`:

```text
Client                                       Server
HELLO username=<base64url>                 -> admit AwaitFirst
                                          <- 401 SCRAM hash=SHA-256, handshakeToken=A
SCRAM handshakeToken=A, data=<first>       -> consume A, preserve exact first-bare
                                          <- 401 SCRAM hash=SHA-256, handshakeToken=B, data=<server-first>
SCRAM handshakeToken=B, data=<proof>       -> consume B, verify proof and registered identity
                                          <- 200 Authentication-Info: authToken=C, hash=SHA-256, data=<verifier>
BEARER authToken=C                         -> protected operation
```

Outer username/data use unpadded base64url; the inner salt/proof/verifier and stored credential records retain standard padded Base64. Client helpers preserve received transcript bytes, validate nonce extension and lengths, and verify the final signature before accepting the bearer. The independent fixture pins canonical RFC 7677 bytes: the Haystack page's illustration omits part of that RFC nonce and includes trailing LF in outer data, so it is not used as a complete cryptographic vector.

The server keeps `AwaitFirst` and `AwaitFinal` phases and bearer records under one lock, with atomic sweep/admission and one-time consumption. Defaults cap handshakes at 1,024 for 60 seconds and bearer records at 4,096 for 3,600 seconds; rotation preserves HELLO's original deadline. Unknown-user decoys use domain-separated HMAC values without per-HELLO PBKDF2. A registered identity remains mandatory even after a valid decoy proof. These bounds do not constitute deployment rate limiting or exhaustive timing qualification.

## Server Request Lifecycle

1. **TCP accept** -- Axum receives the connection
2. **Payload parsing** -- body read up to 2 MB limit
3. **Auth middleware** -- checks Authorization header:
   - `/api/about`, `/api/ops`, `/api/formats` pass through
   - All others require BEARER token (if auth is enabled)
   - Permission check: read / write / admin based on endpoint
4. **Content negotiation** -- `Content-Type` header selects request codec, `Accept` header selects response codec (default: `text/zinc`)
5. **Op handler** -- decodes request grid, executes operation, encodes response grid
6. **Response** -- grid serialized with negotiated codec and returned

## WebSocket Watch Lifecycle

The client and server share a text-only watch JSON profile with string `reqId`
correlation and JSON v3 typed rows. Both cap frames and reassembled messages at
1 MiB. The client atomically admits 1,024 pending calls; one deadline covers
writer queueing, sending, and response waiting. A cancellation guard unregisters
a call and seals the connection if cancellation interrupts a write. Terminal
paths seal admission and settle every waiter before the reader is joined.

Push notifications use a separate client queue of 64 entries. The server also
bounds its outbound queue at 64 and owns the writer through a task set, ensuring
that connection cancellation aborts it. Overflow and protocol failures terminate
the connection explicitly. Reconnection belongs to a later new call; no accepted
operation is automatically replayed. Explicit close permanently disables reconnect.

This slice preserves username ownership in `WatchManager`: a socket disconnect
removes that username's watches across connections. It does not introduce
connection-scoped watches, subscription recovery, or leases. HTTP TLS configuration
also remains separate from the existing public-root WSS connection path.

## Parser DoS Protection

The Zinc/JSON parsers enforce limits to prevent denial-of-service via malicious input:

| Limit | Value |
|-------|-------|
| Nesting depth | 64 levels |
| String size | 10 MB |
| Collection size | 1,000,000 elements |

## Ontology

The `DefNamespace` loads bundled Haystack 4 definitions (`ph`, `phScience`, `phIoT`, `phIct`) and optional Xeto specs. It provides:

- **Taxonomy** -- `is_a(name, supertype)` for nominal subtype checking
- **Membership** -- `entity_is_a(entity, type_name)`: does the entity carry a marker
  for that type or a subtype? This is what a `ph::X` filter term evaluates.
- **Conformance** -- `fits(entity, type_name)`: does the entity carry all of the
  type's mandatory markers? A well-formedness rule, not an identity.
- **Validation** -- `validate_entity(entity)` for ontology conformance
- **Xeto management** -- `load_xeto(source, lib)`, `unload_lib(name)`, `get_spec(qname)`

## Filter Engine

Filter expressions are parsed into an AST (`FilterNode`) by a hand-written recursive descent parser, then evaluated against entities:

```
FilterNode
  +-- Has(path)                    tag exists
  +-- Missing(path)                tag missing
  +-- Cmp { path, op, val }       comparison (==, !=, <, <=, >, >=)
  +-- And(left, right)             logical and
  +-- Or(left, right)              logical or
  +-- SpecMatch(type_name)         ontology type match (e.g., "ph::Ahu")
```

Paths support ref traversal (e.g., `equipRef->siteRef->area`) via a resolver callback. Evaluation short-circuits on And/Or.
