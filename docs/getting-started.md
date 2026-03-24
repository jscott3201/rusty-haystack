# Getting Started

## Prerequisites

- **Rust 1.93+** (edition 2024) -- install via [rustup](https://rustup.rs/)
- **cargo** (included with Rust)
- **Docker** (optional, for containerized deployment)

## Building from Source

Clone the repository and build all crates:

```sh
cargo build --workspace --exclude rusty-haystack
```

The `rusty-haystack` crate (Python bindings) is excluded because it requires maturin and a Python virtual environment. See [Python Bindings](python.md) for setup.

To build only the CLI binary:

```sh
cargo build -p rusty-haystack-cli --release
```

The binary is at `target/release/haystack`.

## Running Tests

```sh
cargo test --workspace --exclude rusty-haystack
```

This runs ~996 tests across all crates.

## Starting the Demo Server

The CLI includes a built-in demo dataset with building automation entities (sites, equips, points):

```sh
cargo run -p haystack-cli -- serve --demo --port 8080
```

Or with the release binary:

```sh
./target/release/haystack serve --demo --port 8080
```

The server starts on `http://127.0.0.1:8080` by default.

## First API Call

Query server info:

```sh
curl http://localhost:8080/api/about
```

List all operations:

```sh
curl http://localhost:8080/api/ops
```

Read all sites (Zinc format):

```sh
curl -X POST http://localhost:8080/api/read \
  -H "Content-Type: text/zinc" \
  -d 'ver:"3.0"
filter
"site"'
```

Read all sites (JSON format):

```sh
curl -X POST http://localhost:8080/api/read \
  -H "Content-Type: application/json" \
  -H "Accept: application/json" \
  -d '{"meta":{"ver":"3.0"},"cols":[{"name":"filter"}],"rows":[{"filter":"site"}]}'
```

Navigate the entity tree:

```sh
curl -X POST http://localhost:8080/api/nav \
  -H "Content-Type: text/zinc" \
  -d 'ver:"3.0"
empty'
```

## Using the CLI

Import entities from a Zinc file and print summary:

```sh
haystack import data/entities.zinc
```

Export entities to JSON:

```sh
cat data/entities.zinc | haystack export --format json
```

Validate entities against the Haystack ontology:

```sh
haystack validate data/entities.zinc
```

Explore the standard library:

```sh
haystack info --def ahu
haystack libs
haystack specs --lib ph
```

See [CLI Reference](cli.md) for the full command reference.

## Docker

Build the image:

```sh
docker build -t rusty-haystack .
```

Run with the demo dataset:

```sh
docker run -p 8080:8080 rusty-haystack serve --demo --port 8080
```

Run with a data file and user authentication:

```sh
docker run -p 8080:8080 \
  -v ./data:/data \
  rusty-haystack serve \
    --file /data/entities.zinc \
    --users /data/users.toml \
    --port 8080
```

See [Configuration](configuration.md) for details on the users TOML format.

## Server with Authentication

1. Create a users file:

```sh
haystack user add admin --file users.toml --password s3cret --permissions read,write,admin
haystack user add viewer --file users.toml --password viewpass --permissions read
```

2. Start the server with auth:

```sh
haystack serve --demo --users users.toml --port 8080
```

3. Authenticate with the client CLI:

```sh
haystack client about --url http://localhost:8080/api -U admin -P s3cret
haystack client read "site" --url http://localhost:8080/api -U admin -P s3cret
```

## Running Benchmarks

```sh
cargo bench -p haystack-core
cargo bench -p haystack-server
```

Results are saved to `target/criterion/`. See [Benchmarks.md](../Benchmarks.md) for published results.

## Next Steps

- [Architecture](architecture.md) -- system design and core abstractions
- [Server API](server-api.md) -- full HTTP endpoint reference
- [Client Library](client.md) -- using HaystackClient in Rust code
- [Configuration](configuration.md) -- server setup, users, permissions
