# Client Library

The `haystack-client` crate provides an async Rust client for communicating with Haystack servers over HTTP and WebSocket.

## Overview

```rust
use haystack_client::{HaystackClient, ClientError};
```

The main type is `HaystackClient<T: Transport>`, generic over HTTP and WebSocket transports. All methods are async and return `Result<HGrid, ClientError>`.

## Connecting

### HTTP

```rust
let client = HaystackClient::connect(
    "http://localhost:8080/api",
    "admin",
    "s3cret",
).await?;
```

Performs SCRAM SHA-256 authentication and returns a client with an embedded bearer token.

### WebSocket

```rust
let client = HaystackClient::connect_ws(
    "http://localhost:8080/api",   // HTTP URL for auth
    "ws://localhost:8080/api/ws",  // WebSocket URL
    "admin",
    "s3cret",
).await?;
```

Authenticates over HTTP first, then upgrades to WebSocket using the obtained token.

### Custom Transport

```rust
let client = HaystackClient::from_transport(my_transport);
```

## Error Handling

```rust
pub enum ClientError {
    AuthFailed(String),
    ServerError(String),
    Transport(String),
    Codec(String),
    ConnectionClosed,
}
```

All methods return `Result<HGrid, ClientError>`. Server-side errors (grids with `err` marker) are converted to `ClientError::ServerError` with the `dis` message.

## HTTP Transport Details

- Default wire format: `text/zinc`
- Custom format: `HttpTransport::with_format(url, token, "application/json")`
- GET for side-effect-free ops (`about`, `ops`, `formats`)
- POST for all other ops
- Authentication: `Authorization: BEARER authToken=<token>`

## WebSocket Transport Details

- Always uses Zinc encoding
- JSON envelope with `id`, `op`, and `body` fields
- Atomic message counter for request/response correlation
- Supports ping/pong heartbeat

## API Methods

### Information

```rust
async fn about(&self) -> Result<HGrid, ClientError>
async fn ops(&self) -> Result<HGrid, ClientError>
async fn formats(&self) -> Result<HGrid, ClientError>
async fn libs(&self) -> Result<HGrid, ClientError>
```

### Read

```rust
async fn read(&self, filter: &str, limit: Option<usize>) -> Result<HGrid, ClientError>
async fn read_by_ids(&self, ids: &[&str]) -> Result<HGrid, ClientError>
```

### Navigation

```rust
async fn nav(&self, nav_id: Option<&str>) -> Result<HGrid, ClientError>
```

### Definitions & Specs

```rust
async fn defs(&self, filter: Option<&str>) -> Result<HGrid, ClientError>
async fn specs(&self, lib: Option<&str>) -> Result<HGrid, ClientError>
async fn spec(&self, qname: &str) -> Result<HGrid, ClientError>
```

### Watch

```rust
async fn watch_sub(&self, ids: &[&str], lease: Option<&str>) -> Result<HGrid, ClientError>
async fn watch_poll(&self, watch_id: &str) -> Result<HGrid, ClientError>
async fn watch_unsub(&self, watch_id: &str, ids: &[&str]) -> Result<HGrid, ClientError>
```

### Point Write

```rust
async fn point_write(&self, id: &str, level: u8, val: Kind) -> Result<HGrid, ClientError>
```

- `level`: priority level 1-17
- `val`: any `Kind` value

### History

```rust
async fn his_read(&self, id: &str, range: &str) -> Result<HGrid, ClientError>
async fn his_write(&self, id: &str, items: Vec<HDict>) -> Result<HGrid, ClientError>
```

`his_write` items are dicts with `ts` (DateTime) and `val` tags.

### Actions

```rust
async fn invoke_action(&self, id: &str, action: &str, args: HDict) -> Result<HGrid, ClientError>
```

### Library Management

```rust
async fn load_lib(&self, name: &str, source: &str) -> Result<HGrid, ClientError>
async fn unload_lib(&self, name: &str) -> Result<HGrid, ClientError>
async fn export_lib(&self, name: &str) -> Result<HGrid, ClientError>
```

### Validation

```rust
async fn validate(&self, entities: Vec<HDict>) -> Result<HGrid, ClientError>
```

### Session

```rust
async fn close_session(&self) -> Result<HGrid, ClientError>
async fn close(&self) -> Result<(), ClientError>
```

- `close_session()` — calls the `close` op to revoke the bearer token
- `close()` — shuts down the transport (no-op for HTTP, sends Close frame for WebSocket)

### Generic Call

```rust
async fn call(&self, op: &str, req: &HGrid) -> Result<HGrid, ClientError>
```

Send any op with a custom request grid.

## Usage Example

```rust
use haystack_client::HaystackClient;
use haystack_core::kinds::Kind;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Connect
    let client = HaystackClient::connect(
        "http://localhost:8080/api",
        "admin",
        "password",
    ).await?;

    // Read sites
    let sites = client.read("site", None).await?;
    for row in sites.iter() {
        println!("{}", row.dis().unwrap_or_default());
    }

    // Read history
    let his = client.his_read("@point-1", "today").await?;
    println!("Got {} history records", his.rows().len());

    // Watch for changes
    let sub = client.watch_sub(&["@site-1", "@equip-1"], None).await?;
    let watch_id = sub.meta().get_str("watchId").unwrap();
    let changes = client.watch_poll(watch_id).await?;

    // Clean up
    client.watch_unsub(watch_id, &[]).await?;
    client.close_session().await?;
    client.close().await?;

    Ok(())
}
```

## Dependencies

| Crate | Purpose |
|-------|---------|
| `haystack-core` | Core types (HGrid, HDict, Kind) |
| `reqwest` | HTTP client (rustls-tls) |
| `tokio-tungstenite` | WebSocket client (rustls-tls) |
| `tokio` | Async runtime |
| `thiserror` | Error derive |
