# Rusty Haystack

A high-performance Rust implementation of the [Project Haystack](https://project-haystack.org) specification for building automation and IoT data modeling.

<!-- Badges -->
<!-- [![Crates.io](https://img.shields.io/crates/v/haystack-core.svg)](https://crates.io/crates/haystack-core) -->
<!-- [![docs.rs](https://docs.rs/haystack-core/badge.svg)](https://docs.rs/haystack-core) -->
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

## Features

- **Full Haystack 4 type system** — all 15 scalar kinds (Marker, Number, Ref, DateTime, Coord, etc.) plus Dict, Grid, and List
- **5 codecs** — Zinc, Trio, JSON (v4), JSON (v3), and CSV with content negotiation
- **Entity graph** — in-memory graph with filter queries, ref traversal, changelog, and concurrent read/write via `SharedGraph`
- **Haystack ontology** — bundled `ph`, `phScience`, `phIoT`, `phIct` definitions with subtype checking and entity validation
- **Xeto type system** — spec parsing, structural fitting, slot resolution with inheritance, and library management
- **HTTP server** — Actix Web 4 with 30+ API endpoints, SCRAM SHA-256 authentication, WebSocket watches, and federation
- **HTTP/WS client** — async client with SCRAM handshake, pluggable transport (HTTP + WebSocket)
- **CLI** — import, export, serve, validate, info, client queries, and user management
- **Python bindings** — PyO3 module with full type coverage (`import rusty_haystack`)
- **Docker** — multi-stage Alpine image for deployment

## Performance

| Operation | Throughput |
|-----------|-----------|
| Zinc encode | ~1,540 rows/ms |
| Zinc decode | ~937 rows/ms |
| Graph lookup | 17 ns per entity (O(1)) |
| Filter (1,000 entities) | ~723 us |
| Ontology fitting | < 1 us |
| HTTP read (single entity) | ~57 us end-to-end |
| HTTP concurrent (50 clients) | ~15 us effective per request |

See [Benchmarks.md](Benchmarks.md) for full results on Apple M2.

## Quick Start

### Prerequisites

- Rust 1.85+ (edition 2024)
- cargo

### Build

```sh
cargo build --workspace --exclude rusty-haystack
```

### Run Tests

```sh
cargo test --workspace --exclude rusty-haystack
```

### Start a Demo Server

```sh
cargo run -p haystack-cli -- serve --demo --port 8080
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

## Workspace Structure

| Crate | Description |
|-------|-------------|
| [`haystack-core`](haystack-core/) | Core library: kinds, data (HGrid/HDict/HCol), codecs, filter, ontology, xeto, graph, auth |
| [`haystack-server`](haystack-server/) | Actix Web HTTP API server with 30+ endpoints, SCRAM auth, WebSocket, watches |
| [`haystack-client`](haystack-client/) | Async HTTP + WebSocket client with SCRAM handshake |
| [`haystack-cli`](haystack-cli/) | CLI binary: import, export, serve, validate, info, client, user |
| [`rusty-haystack`](rusty-haystack/) | PyO3 Python bindings (requires maturin) |

## Documentation

| Document | Description |
|----------|-------------|
| [Architecture](docs/architecture.md) | System design, crate dependencies, core abstractions |
| [Getting Started](docs/getting-started.md) | Build, run, first API call, Docker |
| [Server API](docs/server-api.md) | All HTTP endpoints, auth flow, WebSocket protocol |
| [Client Library](docs/client.md) | HaystackClient API, transports, authentication |
| [CLI Reference](docs/cli.md) | All commands, flags, and examples |
| [Python Bindings](docs/python.md) | Module overview, classes, functions, examples |
| [Configuration](docs/configuration.md) | Server config, users TOML, permissions, Docker |

## License

[MIT](LICENSE)
