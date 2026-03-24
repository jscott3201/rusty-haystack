# Configuration

## Server Configuration

The server is configured entirely via CLI flags when using `haystack serve`:

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--port` | `-p` | TCP port to listen on | `8080` |
| `--host` | | IP address to bind to | `127.0.0.1` |
| `--file` | `-f` | Load entities from a Zinc/Trio/JSON file at startup | Empty graph |
| `--users` | `-u` | TOML file with user credentials for SCRAM auth | Auth disabled |
| `--demo` | | Load a built-in demo building automation dataset | |

When no `--users` file is provided, authentication is disabled and all endpoints are accessible without credentials.

The server password can also be provided via the `HAYSTACK_PASSWORD` environment variable.

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
| `read` | Entity reads, navigation, defs, libs, specs, history reads, watches, exports |
| `write` | Point writes, history writes, actions, imports, library management, validation |
| `admin` | (reserved for future use) |

Public endpoints (no permission needed): `GET /api/about`, `GET /api/ops`, `GET /api/formats`.

## Docker

### Image

The Dockerfile uses a multi-stage build:

1. **Builder stage**: `rust:1.93-alpine` -- compiles the CLI binary with static musl linking
2. **Runtime stage**: `alpine:3.21` -- minimal runtime (~15 MB)

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

The container runs on Alpine Linux. The `HAYSTACK_PASSWORD` environment variable can be used to provide the server password. All other configuration is passed via CLI flags.

## Programmatic Server Configuration

When embedding the server in Rust code, use the builder API:

```rust
use haystack_server::HaystackServer;
use haystack_core::graph::{EntityGraph, SharedGraph};

let graph = SharedGraph::new(EntityGraph::new());

HaystackServer::new(graph)
    .with_namespace(ns)       // DefNamespace
    .with_auth(auth_manager)  // AuthManager
    .with_actions(actions)    // ActionRegistry
    .host("127.0.0.1")
    .port(8080)
    .run()
    .await?;
```
