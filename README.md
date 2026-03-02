# Rusty Haystack

A high-performance Rust implementation of the [Project Haystack](https://project-haystack.org) specification for building automation and IoT data modeling.

<!-- Badges -->
<!-- [![Crates.io](https://img.shields.io/crates/v/haystack-core.svg)](https://crates.io/crates/haystack-core) -->
<!-- [![docs.rs](https://docs.rs/haystack-core/badge.svg)](https://docs.rs/haystack-core) -->
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

## Features

- **Full Haystack 4 type system** — all 15 scalar kinds (Marker, Number, Ref, DateTime, Coord, etc.) plus Dict, Grid, and List
- **6 codecs + RDF** — Zinc, Trio, JSON (v4), JSON (v3), CSV, HBF (Haystack Binary Format) with content negotiation, plus RDF output (Turtle & JSON-LD)
- **HBF binary codec** — serde-based binary format with zstd compression, LEB128 varints, streaming encode support, and 23% faster federation queries vs Zinc text
- **High-performance entity graph** — in-memory graph with bitmap tag indexes, B-tree value indexes, CSR adjacency lists, LRU query cache, filter queries, ref traversal, reactive changelog, and concurrent read/write via `SharedGraph`
- **Expression evaluator** — parse and evaluate arithmetic expressions with variables, built-in functions (min, max, abs, sqrt, clamp), and unit-aware operations for computed points
- **Unit conversion** — Haystack unit database with quantity lookup, compatibility checking, and affine temperature conversions (~10.7M ops/sec)
- **Graph traversal helpers** — hierarchy tree building, entity classification, ref chain walking, parent/child queries, site resolution, and equipment point enumeration
- **Haystack ontology** — bundled `ph`, `phScience`, `phIoT`, `phIct` definitions with subtype checking and entity validation
- **Xeto type system** — spec parsing, structural fitting, slot resolution with inheritance, and library management (scan, load, validate)
- **HLSS snapshots** — graph serialization to Haystack Local Snapshot Store format with zstd compression and CRC32 integrity checks
- **HTTP server** — Actix Web 4 with 43 API endpoints, SCRAM SHA-256 authentication, WebSocket watches with deflate compression, streaming responses for large grids, and role-based access control
- **Graph visualization API** — 6 dedicated endpoints (`graph/flow`, `graph/edges`, `graph/tree`, `graph/neighbors`, `graph/path`, `graph/stats`) returning nodes + edges data optimized for [React Flow](https://reactflow.dev) and similar graph UI libraries
- **Federation** — hub-and-spoke entity aggregation from multiple remote Haystack servers with delta sync, adaptive sync intervals, write forwarding, history fan-out, watch federation, WebSocket-first transport, Arc-based zero-copy entity caching, and mTLS support
- **HTTP/WS client** — async client with SCRAM handshake, pluggable transport (HTTP + WebSocket), backpressure-aware WebSocket, HBF binary support, and mTLS
- **CLI** — import, export, serve, validate, info, libs, specs, client queries, user management, and federation config
- **Python bindings** — PyO3 0.28 module with full API parity: core types, codecs, graph, filter, ontology, client, server, federation, and auth (`import rusty_haystack`)
- **Docker** — multi-stage Alpine image (~15 MB) with a [5-container federation demo](demo/FederatedDemo.md)

## Performance

| Operation | Throughput |
|-----------|-----------|
| Zinc encode | ~1,920 rows/ms |
| Zinc decode | ~921 rows/ms |
| HBF binary encode | ~3,650 rows/ms |
| HBF binary decode | ~1,310 rows/ms |
| Graph lookup | 18 ns per entity (O(1)) |
| Filter (1,000 entities) | ~610 µs |
| Expression eval | ~186 ns (complex, 5 vars) |
| Unit conversion | ~95 ns per convert |
| Ontology fitting | < 1 µs |
| HTTP read (single entity) | ~59 µs end-to-end |
| HTTP concurrent (50 clients) | ~14 µs effective per request |
| Federation filter (20k, HBF) | ~59 ms end-to-end |
| Federation concurrent (50) | ~15 µs effective per request |

See [Benchmarks.md](Benchmarks.md) for full results on Apple M2.

## Quick Start

### Prerequisites

- Rust 1.93+ (edition 2024)
- cargo

### Build

```sh
cargo build --workspace --exclude rusty-haystack
```

### Run Tests

```sh
cargo test --workspace --exclude rusty-haystack
# ~1,320 tests across all crates
```

### Start a Demo Server

```sh
cargo run -p rusty-haystack-cli -- serve --demo --port 8080
```

Then query it:

```sh
curl http://localhost:8080/api/about
curl -X POST http://localhost:8080/api/read \
  -H "Content-Type: text/zinc" \
  -d 'ver:"3.0"
filter
"site"'
```

### Docker

```sh
docker build -t rusty-haystack .
docker run -p 8080:8080 rusty-haystack serve --demo --port 8080
```

### Federation Demo

Run a 5-container federation cluster (1 lead + 4 building nodes):

```sh
cd demo
docker compose up --build
```

See [demo/FederatedDemo.md](demo/FederatedDemo.md) for details.

## Graph Visualization API

Six endpoints under `/api/graph/*` return entity relationship data structured for graph visualization UIs like [React Flow](https://reactflow.dev):

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/graph/flow` | POST | Full graph as nodes + edges with auto-layout positions |
| `/api/graph/edges` | POST | All ref relationships as explicit edge rows |
| `/api/graph/tree` | POST | Recursive subtree from a root entity with depth |
| `/api/graph/neighbors` | POST | N-hop neighborhood around an entity |
| `/api/graph/path` | POST | Shortest path between two entities |
| `/api/graph/stats` | GET | Entity/edge counts, type distribution, connected components |

The `graph/flow` and `graph/neighbors` endpoints return a nodes grid with an edges grid encoded in the metadata — one call gives a UI everything it needs to render. See [Server API docs](docs/server-api.md#graph-visualization-read-permission) for full request/response specs.

## Workspace Structure

| Crate | Description |
|-------|-------------|
| [`haystack-core`](haystack-core/) | Core library: kinds, data (HGrid/HDict/HCol), codecs (Zinc/Trio/JSON/CSV/HBF/RDF), filter engine, expression evaluator, unit conversion, graph with bitmap/B-tree/CSR indexes and traversal helpers, snapshots, ontology, xeto, SCRAM auth |
| [`haystack-server`](haystack-server/) | Actix Web HTTP API server with 43 endpoints, SCRAM auth, WebSocket watches with compression, federation with Arc entity caching, graph visualization, streaming responses |
| [`haystack-client`](haystack-client/) | Async HTTP + WebSocket client with SCRAM handshake, HBF binary support, backpressure, mTLS |
| [`haystack-cli`](haystack-cli/) | CLI binary (`haystack`): import, export, serve, validate, info, libs, specs, client, user management |
| [`rusty-haystack`](rusty-haystack/) | PyO3 0.28 Python bindings with full API parity (requires maturin) |

## Documentation

| Document | Description |
|----------|-------------|
| [Architecture](docs/architecture.md) | System design, crate dependencies, core abstractions |
| [Getting Started](docs/getting-started.md) | Build, run, first API call, Docker |
| [Server API](docs/server-api.md) | All HTTP endpoints, auth flow, WebSocket protocol |
| [Client Library](docs/client.md) | HaystackClient API, transports, authentication |
| [CLI Reference](docs/cli.md) | All commands, flags, and examples |
| [Python Bindings](docs/python.md) | Core types, codecs, graph, filter, client, server, federation |
| [Federation](docs/federation.md) | Federation setup, TOML config, transport, sync, write proxying |
| [Configuration](docs/configuration.md) | Server config, users TOML, permissions, Docker |

## Security

- **No `unsafe` code** — entire codebase is safe Rust
- **SCRAM SHA-256** authentication with PBKDF2 (100k iterations) credential storage
- **Credential zeroization** — `zeroize` crate clears salted passwords, client keys, and SCRAM state from memory on drop
- **Username enumeration prevention** — fake SCRAM challenges for unknown users
- **Constant-time nonce comparison** — prevents timing side-channels during auth
- **Request limits** — 2 MB body size, 100 concurrent watches, 10k IDs per watch, 1M history items
- **WebSocket zip bomb protection** — 10 MB decompressed message limit
- **Filter recursion depth limit** — max depth 100 to prevent stack overflow
- **Expression evaluation limits** — max depth 64, max nodes 1000 to prevent resource exhaustion
- **Federation entity validation** — max 1000 tags per entity, max 256-byte IDs, malformed entities rejected at sync
- **HBF bounds checking** — payload size limits and validated magic headers prevent binary format exploits
- **Xeto loader safety** — symlink traversal protection, file size limits, and directory depth guards
- **Snapshot bomb guard** — decompressed snapshot size limit prevents memory exhaustion
- **Arithmetic overflow protection** — checked operations throughout
- **Sanitized error messages** — internal details not leaked to clients
- **Token rotation** — configurable TTL with automatic expiration

## Key Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| actix-web | 4.13 | HTTP server framework |
| reqwest | 0.13 | HTTP client (rustls TLS) |
| tokio | 1.x | Async runtime |
| tokio-tungstenite | 0.28 | WebSocket client |
| serde | 1.0 | Serialization framework (HBF codec, config) |
| zstd | 0.13 | Zstandard compression for HBF and snapshots |
| rayon | 1.11 | Data parallelism for large graph operations |
| zeroize | 1.x | Secure memory zeroing for credentials |
| pyo3 | 0.28 | Python bindings |
| flate2 | 1.x | Deflate compression for WebSocket |
| criterion | 0.8 | Benchmarking |
| parking_lot | 0.12 | Fast synchronization primitives |

## What's New in v0.6.0

- **HBF (Haystack Binary Format)** — serde-based binary codec with zstd compression, streaming encode, and 23% faster federation queries over HTTP
- **Expression evaluator** — parse and evaluate arithmetic expressions with variables and built-in functions for computed points
- **Unit conversion engine** — Haystack unit database with quantity-aware conversion and affine temperature transforms
- **Graph traversal helpers** — hierarchy trees, ref chain walking, entity classification, and site/equip/point navigation
- **HLSS snapshots** — graph persistence with zstd compression and CRC32 integrity
- **Reactive changelog** — 8,600x faster `changes_since` via version-indexed VecDeque
- **Xeto library management** — scan, load, and validate Xeto spec libraries with safety guards
- **Security hardening** — credential zeroization, entity validation, expression depth limits, symlink protection, snapshot bomb guards
- **Streaming responses** — batched row streaming for large grid responses (>25k rows)
- **Arc entity caching** — zero-copy federation entity sharing eliminates deep clones

## License

[MIT](LICENSE)
