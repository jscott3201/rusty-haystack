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

Performs SCRAM SHA-256 authentication and returns a client with an embedded bearer token. The token is zeroized on drop.

### TLS and HTTP authentication configuration

`connect`, `connect_with_tls`, and `connect_with_config` preserve one configured
HTTP client across the SCRAM exchange and subsequent GET/POST operations. The
same private CA trust and client identity apply when a new TLS connection opens.

```rust
use haystack_client::{AuthMode, ClientConfig, HaystackClient, tls::TlsConfig};

// Add a private CA to normal server trust; no client identity is required.
let tls = TlsConfig::with_ca(std::fs::read("private-ca.pem")?);
let config = ClientConfig {
    tls: Some(tls),
    auth_mode: AuthMode::Scram, // Or Basic for per-request HTTP Basic.
    ..ClientConfig::default()
};
let client = HaystackClient::connect_with_config(
    "https://station.example/api", "user", "password", &config,
).await?;

// For mTLS, load the client certificate/key and an optional CA bundle.
let tls = TlsConfig::from_files("client.pem", "client-key.pem", Some("private-ca.pem"))?;
let client = HaystackClient::connect_with_tls(
    "https://station.example/api", "user", "password", &tls,
).await?;
```

`TlsConfig` accepts both identity buffers empty for CA-only trust; a partial
certificate/key pair is rejected. Additional PEM CA bundles are additive. Default
server-certificate and hostname verification remain enabled. `tls_verify: false`
disables both checks and is intended only for explicitly configured lab use.
`TlsConfig` debug output omits PEM contents, and its owned private key and the
temporary combined PEM buffer are zeroized when dropped.

HTTP Basic is refused over plain HTTP unless `allow_plaintext_basic` is explicitly
set. Basic construction prepares credentials; the first operation verifies them
with the server. SCRAM construction completes authentication before returning.
API URLs must use HTTP or HTTPS and cannot contain userinfo, a query, or a fragment.
Operation names are single alphanumeric/underscore/hyphen segments.

The client follows the [published Haystack authentication exchange](https://project-haystack.org/doc/docHaystack/Auth): `HELLO` sends only the username, the second GET sends SCRAM client-first data, and the third GET sends the proof. Both intermediate responses must be 401; the final response must be 200 and include a verified server signature, bearer token, and `hash=SHA-256`. Outer username/data fields use unpadded base64url. Salt, proof, and verifier inside SCRAM messages use standard padded Base64.

An optional `handshakeToken` belongs to the response that contains it. The client echoes only the immediately preceding response's value, including when a server introduces, removes, or rotates it. Received SCRAM transcript bytes are preserved exactly. SHA-512, PLAINTEXT, and the former two-request wire profile are unsupported.

The default per-request `timeout` and total SCRAM `auth_timeout` are each 30
seconds. The total authentication budget covers all three requests, waiting for a
crypto worker, and proof derivation. The client accepts only SHA-256, rejects
malformed/duplicate/empty fields, limits authentication headers to 8 KiB and 16
values, limits decoded SCRAM data to 4 KiB, and caps PBKDF2 at 1,000,000 iterations.
At most two derivations can run concurrently. Cancelling an already-running
blocking derivation lets that bounded computation finish while retaining its
permit; it cannot send a later authentication request.

Configured HTTP clients do not follow redirects or automatically retry requests.
A lost response after a write does not establish that the write had no effect.
Rejected/expired bearer credentials return `ClientError::AuthFailed`; credentials
are not retained for automatic refresh. Callers can explicitly reconnect with the
same configuration. Low-level `HttpTransport::with_bearer`/`with_basic` and
`auth::authenticate` accept caller-supplied reqwest clients whose redirect, retry,
TLS and timeout policies remain the caller's responsibility.

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
`ClientConfig::tls` and `connect_with_tls` configure HTTP operations only.
`connect_ws` currently has no private-CA or mTLS configuration seam for its
WebSocket connection; these HTTP tests do not qualify configurable WSS.

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
    Connection(String),
    Codec(String),
    ConnectionClosed,
    Timeout(std::time::Duration),
    TooManyRequests,
}
```

Generic grid operations return `Result<HGrid, ClientError>`. HTTP 401/403 responses become
`AuthFailed`; other unsuccessful statuses and grids with an `err` marker become
`ServerError`. HTTP error diagnostics omit response bodies, error-grid `dis` text,
decoder input and URLs so peer-controlled text cannot echo credentials. HTTP
transport failures preserve their category without including the raw request URL.

## HTTP Transport Details

- Default wire format: `text/zinc`
- Custom format: `HttpTransport::with_format(url, token, "application/json")`
- GET for side-effect-free ops (`about`, `ops`, `formats`)
- POST for all other ops
- Authentication: `Authorization: BEARER authToken=<token>`

## WebSocket Transport Details

Only `watchSub`, `watchPoll`, and `watchUnsub` are supported. Requests are uncompressed
JSON text, such as `{"op":"watchSub","reqId":"1","ids":["point-1"]}`. Responses
carry the same string `reqId`, a `watchId` where applicable, and `rows` containing
Haystack JSON v3 typed values. The client converts rows to a grid, derives columns
from their tag names, and places `watchId` in grid metadata. Correlated server
errors become `ClientError::ServerError` with a fixed diagnostic.

- Individual frames and complete reassembled messages are each limited to 1 MiB.
  Binary application messages and application compression are rejected.
- A connection admits at most 1,024 simultaneous requests atomically. The default
  30-second request deadline covers writer queueing, sending, and waiting for a
  response. `WsTransport::connect_with_timeout` changes that budget.
- Malformed messages, I/O loss, and closure terminate the connection and settle
  every pending call. Well-formed responses with unknown, late, or duplicate IDs
  are ignored. Cancellation or timeout after a complete send unregisters only
  that call; cancellation during a write closes the connection because delivery
  may have occurred. Cancelling a call does not undo any server-side effect.
- Unsolicited `{"type":"push","watchId":"...","rows":[...]}` notifications are
  available through `client.next_watch_push()` or `WsTransport::next_push()`.
  The queue holds 64 notifications; overflow closes the connection explicitly.
- `close()` rejects new work, settles pending calls, and joins the reader within
  its bounded shutdown path. Ping/pong control frames remain supported.
- `ReconnectingWsTransport` reconnects once before a new call when its prior
  connection is terminal. It never replays a dispatched call and never reconnects
  after explicit close. A new connection does not restore subscriptions.

`watchSub` creates a new watch from 1–1,000 IDs. `lease`, `watchDis`, existing-watch
subscription, generic operations, and extra request metadata are rejected locally.
`watchPoll` requires a watch ID and no rows. `watchUnsub` with an empty ID list
removes the watch; a nonempty list removes those IDs and leaves the watch alive,
even if no IDs remain. IDs are nonempty after stripping one leading `@` and are
limited to 1,024 bytes; watch IDs are nonempty and limited to 128 bytes.

Watch ownership remains username-scoped on the server. Disconnecting one socket
removes that user's watches, including watches used by another connection. Push
routing also follows username ownership; connection isolation is not provided.

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

- `close_session()` -- calls the `close` op to revoke the bearer token
- `close()` -- shuts down the transport (no-op for HTTP, sends Close frame for WebSocket)

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

## Entity mutation extension

The typed `submit_entities`, `reconcile_entity` and `entity_changes` methods share
core entity-v1 DTOs with embedding and HTTP. They preserve rich values through a
typed-v1 STR envelope and unsigned revisions through decimal strings. A possible
submission followed by a transport/body/invalid-ack failure returns `Unknown`
with the original operation identity. Reconcile explicitly; no mutation POST is
automatically retried. Use first-party HTTP constructors with known no-retry and
no-redirect policies. Caller-supplied reqwest clients cannot qualify submission;
`HttpTransport::with_bearer_config` supports custom first-party configuration.
WebSocket has no entity mutation extension. See [entity contracts](entity-mutations.md).

## Bounded history extension

`his_read_scoped(&HistoryReadRequest)` selects the bounded-history-v1 profile
on a `HistoryTransport`. It returns typed schema, independent graph/history
observations, retained coverage, and explicit Complete/Limited/Interrupted/Failed
alongside any partial rows. The helper rejects missing, foreign or contradictory
terminal metadata and does not retry. First-party HTTP constructors collect one
bounded response in Zinc or JSON v3/v4. No remote pull protocol is implied.
The existing `his_read` remains an explicit legacy grid call; Python and CLI
helpers do not silently opt into partial results. See [history reads](history-reads.md).
