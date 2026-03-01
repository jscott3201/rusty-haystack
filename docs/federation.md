# Federation

Federation allows a Haystack server to aggregate entities from multiple remote Haystack servers into a single unified namespace. Remote entities are cached locally, prefixed with a configurable ID namespace, and kept fresh via background sync tasks. Clients query one lead server and transparently receive data from all federated nodes.

**Key capabilities:**

- Read operations merge local and federated entities seamlessly
- Write operations (pointWrite, hisWrite, invokeAction, import) are automatically proxied to the owning remote server
- History reads (hisRead) fan out to the correct remote
- Watches can track federated entities via cache-based polling
- WebSocket-first transport with HTTP fallback per connector
- mTLS support for secure environments

## Quick Start

1. Create a federation TOML file:

```toml
[connectors.building-a]
name = "Building A"
url = "http://building-a:8080/api"
username = "federation"
password = "s3cret"
id_prefix = "bldg-a-"
sync_interval_secs = 30

[connectors.building-b]
name = "Building B"
url = "https://building-b:8443/api"
username = "federation"
password = "s3cret"
id_prefix = "bldg-b-"
```

2. Start the server with the `--federation` flag:

```sh
haystack serve --file entities.zinc --users users.toml --federation federation.toml
```

The server loads the TOML, creates a connector for each entry, connects to each remote (WS-first, HTTP fallback), syncs all entities into local caches, and starts background sync tasks. On startup you'll see:

```
Loaded 2 federation connectors
```

3. Verify federation status:

```sh
curl http://localhost:8080/api/federation/status -H "Accept: application/json"
```

## TOML Configuration

Federation is configured through a TOML file passed via the `--federation` CLI flag. Each connector is defined under `[connectors.<key>]` where `<key>` is an arbitrary identifier (used only as the TOML key, not exposed at runtime — the `name` field is the display name).

### ConnectorConfig Fields

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | String | Yes | — | Display name shown in status endpoint and logs |
| `url` | String | Yes | — | Base URL of the remote Haystack API (e.g. `http://remote:8080/api`) |
| `username` | String | Yes | — | Username for SCRAM SHA-256 authentication on the remote |
| `password` | String | Yes | — | Password for SCRAM SHA-256 authentication on the remote |
| `id_prefix` | String | No | None | Prefix applied to all entity Ref values (e.g. `"bldg-a-"`). See [ID Prefixing](#id-prefixing) |
| `ws_url` | String | No | Derived from `url` | WebSocket URL override (e.g. `ws://remote:8080/api/ws`). See [Transport](#transport) |
| `sync_interval_secs` | Integer | No | `60` | Background sync interval in seconds. Lower values increase freshness but also load on the remote |
| `client_cert` | String | No | None | Path to PEM client certificate for mTLS. See [mTLS](#mtls) |
| `client_key` | String | No | None | Path to PEM client private key for mTLS |
| `ca_cert` | String | No | None | Path to PEM CA certificate for custom server verification |

### Full Example

```toml
[connectors.building-a]
name = "Building A"
url = "http://building-a:8080/api"
username = "federation"
password = "s3cret"
id_prefix = "bldg-a-"
sync_interval_secs = 30

[connectors.building-b]
name = "Building B"
url = "https://building-b:8443/api"
username = "federation"
password = "s3cret"
id_prefix = "bldg-b-"
client_cert = "/etc/certs/federation.pem"
client_key = "/etc/certs/federation-key.pem"
ca_cert = "/etc/certs/ca.pem"
```

### Programmatic Configuration

When embedding the server in Rust code, build the federation directly:

```rust
use haystack_server::Federation;
use haystack_server::connector::ConnectorConfig;

let mut fed = Federation::new();
fed.add(ConnectorConfig {
    name: "Building A".into(),
    url: "http://building-a:8080/api".into(),
    username: "federation".into(),
    password: "s3cret".into(),
    id_prefix: Some("bldg-a-".into()),
    ws_url: None,
    sync_interval_secs: Some(30),
    client_cert: None,
    client_key: None,
    ca_cert: None,
});

// Or load from a TOML string/file:
let fed = Federation::from_toml_str(toml_str)?;
let fed = Federation::from_toml_file("federation.toml")?;
```

## ID Prefixing

When `id_prefix` is set, all Ref values in synced entities are prefixed during the sync step. This prevents ID collisions when multiple remotes have entities with the same IDs.

### What Gets Prefixed

- The `id` tag
- Any tag whose name ends with `Ref` (e.g. `siteRef`, `equipRef`, `floorRef`, `spaceRef`)
- Only tags whose values are actually `Kind::Ref` — non-Ref values with Ref-like names are left unchanged
- The `dis` metadata on Refs is preserved

### Example

With `id_prefix = "bldg-a-"`:

| Remote Value | Federated Value |
|-------------|-----------------|
| `id: @site-1` | `id: @bldg-a-site-1` |
| `siteRef: @site-1` | `siteRef: @bldg-a-site-1` |
| `equipRef: @ahu-1` | `equipRef: @bldg-a-ahu-1` |
| `dis: "Demo Site"` | `dis: "Demo Site"` (unchanged) |

### Prefix Stripping

When proxying writes back to the remote, the prefix is stripped automatically using the inverse `strip_prefix_refs` operation. Each proxy method calls `strip_id()` which removes the configured prefix from the entity ID before forwarding. For `proxy_import()`, the full entity dict has all Ref prefixes stripped.

## Transport

### WS-First with HTTP Fallback

Each connector attempts to establish a WebSocket connection first via `HaystackClient::connect_ws()`. If the WS connection fails (e.g. the remote doesn't support WebSocket, or a proxy blocks it), the connector falls back to HTTP via `HaystackClient::connect()`.

The active transport mode (`"http"` or `"ws"`) is tracked per connector and reported in the federation status endpoint.

### WebSocket URL Derivation

The WebSocket URL is derived automatically from the `url` field by replacing the scheme and appending `/ws`:

| HTTP URL | Derived WS URL |
|----------|---------------|
| `http://host:8080/api` | `ws://host:8080/api/ws` |
| `https://host:8443/api` | `wss://host:8443/api/ws` |

To override the derived URL, set `ws_url` explicitly in the connector config.

### Persistent Connections

Connectors maintain a persistent client connection (either WS or HTTP) stored in an `Arc<tokio::sync::RwLock<Option<ConnectorClient>>>`. The client is reused across sync cycles and proxy calls.

If a connection error occurs during sync, the persistent client is cleared and re-established on the next sync iteration. This auto-recovery pattern means transient network issues are handled gracefully without manual intervention.

### Proxy Connections

Proxy methods (hisRead, pointWrite, hisWrite, invokeAction, import) create a fresh HTTP connection via `connect_remote()` rather than reusing the persistent client. This avoids contention between sync tasks and proxy calls.

## Background Sync

When the server starts with federation configured, `Federation::start_background_sync()` spawns one tokio task per connector. Each task runs an infinite loop:

1. **Connect** to the remote (WS-first, HTTP fallback) if not already connected
2. **Read** all entities via a `read` op with filter `"*"`
3. **Prefix** all Ref values if `id_prefix` is configured
4. **Replace** the local entity cache atomically and rebuild the ownership index (a `HashSet<String>` of all owned IDs)
5. **Update** the `last_sync` timestamp and set `connected = true`
6. **Sleep** for `sync_interval_secs` (default: 60 seconds)

On sync failure, the persistent client is cleared to force reconnection on the next cycle, and `connected` is set to `false`.

### Per-Connector Sync

In addition to background sync, you can trigger a manual sync via the HTTP API:

```sh
# Sync all connectors
curl -X POST http://localhost:8080/api/federation/sync \
  -H "Accept: application/json"

# Sync a single connector by display name
curl -X POST http://localhost:8080/api/federation/sync/Building%20A \
  -H "Accept: application/json"
```

Both return a grid with columns: `name` (Str), `result` (Str — entity count or error message), `ok` (Bool).

Sync endpoints require **write** permission.

## Entity Ownership

Each connector maintains an ownership index — a `HashSet<String>` of all entity IDs in its cache. This index is rebuilt atomically on every sync cycle.

The `Federation::owner_of(id)` method iterates over all connectors and returns the first one whose `owns(id)` returns `true`. This enables:

- **Read proxy**: When reading by ID, if the entity is not in the local graph but is owned by a connector, the cached entity is returned.
- **Write routing**: Write operations are automatically proxied to the owning connector when the target entity is not in the local graph.
- **History fan-out**: hisRead requests for federated entities are proxied to the owning remote server.

## Read Operations

### Filter-Based Read

The `read` op with a `filter` column queries both the local graph and all federated connector caches:

1. Evaluate the filter against the local entity graph
2. If results are below the `limit`, fetch all cached entities from all connectors via `Federation::all_cached_entities()`
3. Parse the filter expression and evaluate it against each federated entity
4. Merge federated matches until the limit is reached

Local results always appear first, followed by federated entities.

### ID-Based Read

The `read` op with an `id` column checks in order:

1. The local entity graph
2. If not found locally, the federation ownership index via `Federation::owner_of(id)`
3. If a connector owns the ID, search its cached entities for the matching entity
4. If still not found, return a missing-entity row containing only the requested ID

## Write Forwarding

When a write operation targets an entity that is not in the local graph but is owned by a federated connector, the request is automatically proxied to the remote server. The ID prefix is stripped before forwarding so the remote receives its original entity IDs.

| Operation | Proxy Method | Behavior |
|-----------|-------------|----------|
| `pointWrite` | `proxy_point_write()` | Forwards `id`, `level`, `val` to the remote |
| `hisWrite` | `proxy_his_write()` | Forwards all `ts`/`val` rows to the remote |
| `invokeAction` | `proxy_invoke_action()` | Forwards `id`, `action`, and argument dict to the remote |
| `import` | `proxy_import()` | Strips all Ref prefixes from the entity dict, wraps in a single-row grid, sends to remote `import` op |

If the entity is not found in the local graph **and** not owned by any connector, a 404 error is returned.

## History Fan-Out

The `hisRead` op checks whether the requested entity is local or federated:

1. If the entity is in the local graph, the local `HisStore` is queried as usual with the parsed date range.
2. If the entity is not local but is owned by a federated connector, the request is proxied to the remote via `proxy_his_read()`. The ID prefix is stripped and the `range` string is forwarded as-is.

The response grid from the remote is returned directly to the client without transformation.

## Watch Federation

Watches can include both local and federated entity IDs. The federation uses cache-based polling rather than real-time push subscriptions.

### Subscribe (watchSub)

When subscribing to a watch:

1. Local entities are resolved from the graph as usual
2. For each ID not found locally, check the federation ownership index
3. If a connector owns the ID, look up the entity in the connector's cache
4. Include the cached entity in the initial subscription response
5. Register the ID in the connector's `remote_watch_ids` set

### Poll (watchPoll)

Polling returns changed entities from the local `WatchManager`. Federation changes are detected when background sync updates the connector cache — the next poll will return updated entities for any watched federated IDs.

### Unsubscribe (watchUnsub)

When unsubscribing specific IDs (or removing the entire watch), the corresponding entries in `remote_watch_ids` are cleaned up on the owning connector.

## mTLS

For environments that require mutual TLS, configure the `client_cert`, `client_key`, and optional `ca_cert` fields on the connector:

```toml
[connectors.secure-building]
name = "Secure Building"
url = "https://secure:8443/api"
username = "federation"
password = "s3cret"
id_prefix = "sec-"
client_cert = "/etc/certs/federation.pem"
client_key = "/etc/certs/federation-key.pem"
ca_cert = "/etc/certs/ca.pem"
```

The client library provides `TlsConfig` and `HaystackClient::connect_with_tls()` for programmatic mTLS connections. When `ca_cert` is provided, it is used for server certificate verification instead of the system trust store.

## Status Endpoint

`GET /api/federation/status` returns a grid with one row per connector:

| Column | Type | Description |
|--------|------|-------------|
| `name` | Str | Connector display name |
| `entityCount` | Number | Number of cached entities |
| `transport` | Str | Active transport: `"http"` or `"ws"` |
| `connected` | Bool | Whether the last sync succeeded |
| `lastSync` | DateTime | Timestamp of the last successful sync (Null if never synced) |

Example response (JSON):

```json
{
  "cols": [
    {"name": "name"}, {"name": "entityCount"}, {"name": "transport"},
    {"name": "connected"}, {"name": "lastSync"}
  ],
  "rows": [
    {
      "name": "s:Building A",
      "entityCount": "n:36",
      "transport": "s:ws",
      "connected": "m:",
      "lastSync": "t:2026-03-01T12:00:00Z UTC"
    }
  ]
}
```

## Authentication

Each connector authenticates to its remote server using SCRAM SHA-256 (the same protocol the server uses for client authentication). The `username` and `password` in the connector config are used for the SCRAM handshake.

Create a dedicated federation user on each remote server:

```sh
# Read-only (sufficient for sync)
haystack user add federation --file users.toml --password s3cret --permissions read

# Read+write (required if using write forwarding)
haystack user add federation --file users.toml --password s3cret --permissions read,write
```

The federation user needs:
- **`read` permission** to sync entities (required)
- **`write` permission** if write forwarding is used (pointWrite, hisWrite, invokeAction, import)

## Rust API Reference

### Federation

| Method | Signature | Description |
|--------|-----------|-------------|
| `new()` | `-> Self` | Create empty federation with no connectors |
| `add()` | `(&mut self, config: ConnectorConfig)` | Add a connector from config |
| `from_toml_str()` | `(s: &str) -> Result<Self, String>` | Parse federation from TOML string |
| `from_toml_file()` | `(path: &str) -> Result<Self, String>` | Load federation from TOML file |
| `owner_of()` | `(&self, id: &str) -> Option<&Arc<Connector>>` | Find connector that owns an entity ID |
| `all_cached_entities()` | `(&self) -> Vec<HDict>` | Merge all cached entities from all connectors |
| `connector_count()` | `(&self) -> usize` | Number of configured connectors |
| `status()` | `(&self) -> Vec<(String, usize)>` | Get (name, entity_count) pairs |
| `sync_one()` | `(&self, name: &str) -> Result<usize, String>` | Manually sync a single connector (async) |
| `sync_all()` | `(&self) -> Vec<(String, Result<usize, String>)>` | Sync all connectors (async) |
| `start_background_sync()` | `(&self) -> Vec<JoinHandle<()>>` | Spawn background sync tasks |

### Connector

| Method | Signature | Description |
|--------|-----------|-------------|
| `sync()` | `(&self) -> Result<usize, String>` | Sync entities from remote (async) |
| `owns()` | `(&self, id: &str) -> bool` | Check if this connector owns an entity ID |
| `cached_entities()` | `(&self) -> Vec<HDict>` | Get all cached entities |
| `entity_count()` | `(&self) -> usize` | Count of cached entities |
| `is_connected()` | `(&self) -> bool` | Whether last sync succeeded |
| `transport_mode()` | `(&self) -> TransportMode` | Current transport (Http or WebSocket) |
| `last_sync_time()` | `(&self) -> Option<DateTime<Utc>>` | Timestamp of last successful sync |
| `proxy_his_read()` | `(&self, id: &str, range: &str) -> Result<HGrid, String>` | Proxy hisRead to remote (async) |
| `proxy_point_write()` | `(&self, id: &str, level: u8, val: &Kind) -> Result<HGrid, String>` | Proxy pointWrite (async) |
| `proxy_his_write()` | `(&self, id: &str, items: Vec<HDict>) -> Result<HGrid, String>` | Proxy hisWrite (async) |
| `proxy_import()` | `(&self, entity: &HDict) -> Result<HGrid, String>` | Proxy import (async) |
| `proxy_invoke_action()` | `(&self, id: &str, action: &str, args: HDict) -> Result<HGrid, String>` | Proxy invokeAction (async) |

### ConnectorConfig

| Method | Signature | Description |
|--------|-----------|-------------|
| `effective_ws_url()` | `(&self) -> String` | Return `ws_url` or derive from `url` |
| `effective_sync_interval_secs()` | `(&self) -> u64` | Return `sync_interval_secs` or default 60 |

## Docker Demo

A ready-to-run 5-container Docker Compose demo is available in the [`demo/`](../demo/) directory. See [`demo/FederatedDemo.md`](../demo/FederatedDemo.md) for setup instructions.

The demo runs 1 lead node + 4 building nodes, each with built-in demo data (36 entities per node), federated into a single namespace of ~180 entities.

## Limitations

- **No cascading federation**: A federated remote server cannot itself federate to other servers through this server. Federation is single-hop only.
- **Full sync**: Each sync cycle fetches all entities from the remote (`read` with filter `"*"`). There is no incremental/delta sync. For large deployments, consider increasing `sync_interval_secs` to reduce load.
- **Cache-based watches**: Watch federation relies on background sync polling rather than real-time push subscriptions to the remote server. Watch update latency is bounded by the sync interval.
- **Proxy connections**: Proxy methods (write forwarding, hisRead) create a fresh HTTP connection per call rather than reusing the persistent sync client. This avoids contention but adds per-request connection overhead.
