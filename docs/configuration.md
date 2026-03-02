# Configuration

## Server Configuration

The server is configured entirely via CLI flags when using `haystack serve`:

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--port` | `-p` | TCP port to listen on | `8080` |
| `--host` | | IP address to bind to | `0.0.0.0` |
| `--file` | `-f` | Load entities from a Zinc/Trio/JSON file at startup | Empty graph |
| `--users` | `-u` | TOML file with user credentials for SCRAM auth | Auth disabled |
| `--demo` | | Load a built-in demo building automation dataset | |
| `--federation` | | TOML file with federation connector configuration | No federation |

When no `--users` file is provided, authentication is disabled and all endpoints are accessible without credentials.

## Users TOML Format

User credentials are stored in a TOML file. Each user has a password hash and permissions assigned via roles or explicit lists.

### Basic Example

```toml
[users.admin]
password_hash = "<hash>"
role = "admin"

[users.operator]
password_hash = "<hash>"
role = "operator"

[users.viewer]
password_hash = "<hash>"
role = "viewer"
```

### Creating Password Hashes

Use the CLI to manage users (it handles hashing automatically):

```sh
# Add a user
haystack user add admin --file users.toml --password s3cret --permissions read,write,admin

# Add a viewer
haystack user add viewer --file users.toml --password viewpass

# Change password
haystack user passwd admin --file users.toml --password newpass

# List users
haystack user list --file users.toml

# Delete a user
haystack user delete viewer --file users.toml
```

### Password Hash Format

Hashes use the format: `base64(salt):iterations:base64(stored_key):base64(server_key)`

- 16-byte random salt
- 100,000 PBKDF2-HMAC-SHA-256 iterations
- 32-byte stored key and server key

### Built-in Roles

| Role | Permissions |
|------|-------------|
| `admin` | read, write, admin |
| `operator` | read, write |
| `viewer` | read |

### Explicit Permissions

Instead of roles, you can set permissions directly:

```toml
[users.custom]
password_hash = "<hash>"
permissions = ["read", "write"]
```

When both `role` and `permissions` are present, `permissions` takes precedence.

### User Entry Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `password_hash` | String | Yes | SCRAM credential hash |
| `role` | String | No | Built-in role name: `admin`, `operator`, `viewer` |
| `permissions` | Array | No | Explicit permission list: `read`, `write`, `admin` |

If neither `role` nor `permissions` is set, the user has no permissions (cannot access any endpoint).

## Permission Model

| Permission | Grants Access To |
|------------|-----------------|
| `read` | Entity reads, navigation, defs, libs, specs, history reads, watches, exports, RDF, federation status |
| `write` | Point writes, history writes, actions, imports, library management, validation, federation sync |
| `admin` | System status, backup, restore |

Public endpoints (no permission needed): `GET /api/about`, `GET /api/ops`, `GET /api/formats`.

## Federation TOML Format

Federation connectors are configured in a TOML file passed via `--federation`. Each connector is defined under `[connectors.<key>]` where `<key>` is an arbitrary identifier.

### Example

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

### Connector Fields

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | String | Yes | | Display name for this connector |
| `url` | String | Yes | | Base URL of the remote Haystack API |
| `username` | String | Yes | | Username for SCRAM auth on the remote |
| `password` | String | Yes | | Password for SCRAM auth on the remote |
| `id_prefix` | String | No | None | Prefix for all entity Ref values from this remote |
| `ws_url` | String | No | Derived from `url` | WebSocket URL override |
| `sync_interval_secs` | Integer | No | `60` | Background sync interval in seconds |
| `client_cert` | String | No | None | Path to PEM client certificate for mTLS |
| `client_key` | String | No | None | Path to PEM client private key for mTLS |
| `ca_cert` | String | No | None | Path to PEM CA certificate for server verification |

See [Federation](federation.md) for full details on transport, sync behavior, write forwarding, and watch federation.

## Docker

### Image

The Dockerfile uses a multi-stage build:

1. **Builder stage**: `rust:1.93-alpine` — compiles the CLI binary with static musl linking
2. **Runtime stage**: `alpine:3.21` — minimal runtime (~15 MB)

```sh
docker build -t rusty-haystack .
```

### Running

```sh
# Demo server
docker run -p 8080:8080 rusty-haystack serve --demo --port 8080

# With data and auth
docker run -p 8080:8080 \
  -v ./data:/data \
  rusty-haystack serve \
    --file /data/entities.zinc \
    --users /data/users.toml \
    --port 8080
```

### Defaults

- **Entrypoint**: `haystack`
- **Default command**: `serve --port 8080`
- **Exposed port**: `8080`

Override the command to use any CLI subcommand:

```sh
docker run rusty-haystack info --def ahu
docker run rusty-haystack validate /data/entities.zinc
```

### Environment

The container runs on Alpine Linux. No environment variables are required. All configuration is passed via CLI flags.

## Programmatic Server Configuration

When embedding the server in Rust code, use the builder API:

```rust
use haystack_server::{HaystackServer, Federation};
use haystack_core::graph::{EntityGraph, SharedGraph};

let graph = SharedGraph::new(EntityGraph::new());

// Load federation from TOML file or build programmatically
let fed = Federation::from_toml_file("federation.toml")?;

HaystackServer::new(graph)
    .with_namespace(ns)       // DefNamespace
    .with_auth(auth_manager)  // AuthManager
    .with_actions(actions)    // ActionRegistry
    .with_federation(fed)     // Federation
    .host("0.0.0.0")
    .port(8080)
    .run()
    .await?;
```
