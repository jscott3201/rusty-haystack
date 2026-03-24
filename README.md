# Rusty Haystack

A high-performance Rust implementation of the [Project Haystack](https://project-haystack.org) specification for building automation and IoT data modeling.

<!-- Badges -->
<!-- [![Crates.io](https://img.shields.io/crates/v/haystack-core.svg)](https://crates.io/crates/haystack-core) -->
<!-- [![docs.rs](https://docs.rs/haystack-core/badge.svg)](https://docs.rs/haystack-core) -->
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

## Features

- **Full Haystack 4 type system** — all 15 scalar kinds (Marker, Number, Ref, DateTime, Coord, etc.) plus Dict, Grid, and List
- **5 codecs** — Zinc 3.0, JSON v4, JSON v3, Trio, CSV with content negotiation
- **High-performance entity graph** — in-memory EntityGraph with RoaringBitmap tag indexes, B-tree value indexes, bidirectional ref adjacency, reactive changelog for watches, and concurrent read/write via `SharedGraph`
- **Haystack filter engine** — parse and evaluate Haystack filter expressions with path traversal
- **Unit conversion** — Haystack unit database with quantity lookup, compatibility checking, and affine temperature conversions
- **Graph traversal helpers** — hierarchy tree building, entity classification, ref chain walking, parent/child queries, site resolution, and equipment point enumeration
- **Haystack ontology** — bundled `ph`, `phScience`, `phIoT`, `phIct` definitions with taxonomy, subtype checking, and entity validation
- **Xeto type system** — schema language parser, structural type fitting, slot resolution with inheritance, spec resolution, and library management (scan, load, validate)
- **HTTP server** — Axum 0.8 with 25 API endpoints, Tower auth middleware, SCRAM SHA-256 authentication, WebSocket watches, role-based access control, and 2 MB body limit
- **HTTP/WS client** — async client with SCRAM handshake, mTLS, token zeroization, and pluggable transport (HTTP + WebSocket)
- **CLI** — import, export, serve, validate, info, libs, specs, client queries, and user management. Password via `HAYSTACK_PASSWORD` env var.
- **Python bindings** — PyO3 0.28 module with types, codecs, graph, filter, ontology, Xeto, client, server, and auth (`import rusty_haystack`)
- **Docker** — multi-stage Alpine image (~15 MB)

## Performance

| Operation | Throughput |
|-----------|-----------|
| Zinc encode | ~1,920 rows/ms |
| Zinc decode | ~921 rows/ms |
| Graph lookup | 18 ns per entity (O(1)) |
| Filter (1,000 entities) | ~610 us |
| Unit conversion | ~95 ns per convert |
| Ontology fitting | < 1 us |
| HTTP read (single entity) | ~59 us end-to-end |
| HTTP concurrent (50 clients) | ~14 us effective per request |

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
# ~970 tests across all crates
```

### Start a Demo Server

```sh
cargo run -p rusty-haystack-cli -- serve --demo --port 8080
```

The server binds to `127.0.0.1` by default. To listen on all interfaces, pass `--host 0.0.0.0`.

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

## Workspace Structure

| Crate | Description |
|-------|-------------|
| [`haystack-core`](haystack-core/) | Core library: kinds, data (HGrid/HDict/HCol), codecs (Zinc/Trio/JSON/CSV), filter engine, unit conversion, graph with RoaringBitmap/B-tree indexes and ref adjacency, ontology, Xeto, SCRAM auth |
| [`haystack-server`](haystack-server/) | Axum HTTP API server with 25 endpoints, Tower auth middleware, SCRAM auth, WebSocket watches |
| [`haystack-client`](haystack-client/) | Async HTTP + WebSocket client with SCRAM handshake, mTLS, token zeroization |
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
| [Python Bindings](docs/python.md) | Core types, codecs, graph, filter, client, server, auth |
| [Configuration](docs/configuration.md) | Server config, users TOML, permissions, Docker |

## Security

- **No `unsafe` code** — entire codebase is safe Rust
- **SCRAM SHA-256** authentication with PBKDF2 (100k iterations) credential storage
- **Credential zeroization** — `zeroize` crate clears salted passwords, client keys, and SCRAM state from memory on drop (both server and client)
- **Username enumeration prevention** — fake SCRAM challenges for unknown users
- **Constant-time comparison** — prevents timing side-channels during auth
- **Request limits** — 2 MB body size, 100 concurrent watches, 10k IDs per watch, 1M history items
- **Filter recursion depth limit** — max depth 100 to prevent stack overflow
- **Parser DoS protection** — depth limits and size limits on all parsers
- **Xeto loader safety** — symlink traversal protection, file size limits, and directory depth guards
- **Token TTL** — configurable TTL with periodic cleanup of expired tokens
- **Arithmetic overflow protection** — checked operations throughout
- **Sanitized error messages** — internal details not leaked to clients

## Key Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| axum | 0.8 | HTTP server framework |
| tower / tower-http | 0.5 / 0.6 | Middleware (auth, CORS, body limits) |
| reqwest | 0.13 | HTTP client (rustls TLS) |
| tokio | 1.x | Async runtime |
| tokio-tungstenite | 0.28 | WebSocket client |
| serde / serde_json | 1.0 | Serialization framework |
| roaring | 0.10 | Compressed bitmap indexes (RoaringBitmap) |
| parking_lot | 0.12 | Fast synchronization primitives |
| zeroize | 1.x | Secure memory zeroing for credentials |
| pyo3 | 0.28 | Python bindings |
| criterion | 0.8 | Benchmarking |
| rustc-hash | 2 | Fast non-cryptographic hashing (FxHasher) |

## What's New in v0.8.0

v0.8.0 is a major simplification of the codebase, removing federation, the HBF binary codec, RDF output, the expression evaluator, HLSS snapshots, graph visualization endpoints, and CSR adjacency. The server framework was migrated from Actix Web to Axum.

**Removed:**
- Federation (hub-and-spoke, connectors, delta sync, write forwarding, watch federation)
- HBF codec (Haystack Binary Format, zstd compression, binary encode/decode)
- RDF output (Turtle, JSON-LD)
- Arrow IPC codec
- Expression evaluator (arithmetic expressions, variables, functions)
- HLSS snapshots (graph serialization, snapshot commands)
- Graph visualization API (6 endpoints under `/api/graph/*`)
- System management endpoints (backup/restore/status)
- CSR adjacency, columnar storage, query planner, WL structural fingerprinting
- Streaming responses
- Dependencies: rayon, dashmap, flate2, zstd, crc32fast

**Changed:**
- Server framework migrated from Actix Web 4 to Axum 0.8 with Tower middleware
- CLI defaults to `127.0.0.1` (was `0.0.0.0`)
- Password can be set via `HAYSTACK_PASSWORD` environment variable
- 5 codecs remain: Zinc 3.0, JSON v4, JSON v3, Trio, CSV
- 25 API endpoints (standard Haystack ops plus extensions)

## License

[MIT](LICENSE)
