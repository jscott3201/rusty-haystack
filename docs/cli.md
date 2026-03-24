# CLI Reference

The `haystack` CLI binary provides commands for serving, importing, exporting, validating data, and querying remote servers.

```sh
cargo build -p haystack-cli --release
./target/release/haystack --help
```

## Commands

### `serve`

Start the Haystack HTTP API server.

```sh
haystack serve [OPTIONS]
```

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--port` | `-p` | Port to listen on | `8080` |
| `--host` | | Host to bind to | `127.0.0.1` |
| `--file` | `-f` | Load entities from file at startup | |
| `--users` | `-u` | TOML file with user credentials for SCRAM auth | |
| `--demo` | | Load a demo building automation dataset | |

The server password can also be provided via the `HAYSTACK_PASSWORD` environment variable, which is useful for containerized deployments where CLI flags are less convenient.

Examples:

```sh
# Demo server
haystack serve --demo

# Production with auth
haystack serve --file data/entities.zinc --users users.toml --port 9090

# Bind to all interfaces (e.g., for Docker)
haystack serve --demo --host 0.0.0.0 --port 8080

# Password via environment variable
HAYSTACK_PASSWORD=s3cret haystack serve --file data/entities.zinc --users users.toml
```

### `import`

Import entities from a file and print summary.

```sh
haystack import <FILE> [OPTIONS]
```

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--format` | `-f` | Input format: `zinc`, `trio`, `json`, `json3` | Auto-detect from extension |

Examples:

```sh
haystack import data/entities.zinc
haystack import data/entities.json --format json
```

### `export`

Export entities to a specified format. Reads from stdin.

```sh
haystack export [OPTIONS]
```

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--format` | `-f` | Output format: `zinc`, `trio`, `json`, `json3` | `zinc` |
| `--output` | `-o` | Output file path (stdout if omitted) | |
| `--filter` | | Filter expression to select entities | |

Examples:

```sh
cat data/entities.zinc | haystack export --format json
cat data/entities.zinc | haystack export --format trio --output out.trio
cat data/entities.zinc | haystack export --filter "site" --format json
```

### `validate`

Validate entities against the standard Haystack ontology.

```sh
haystack validate <FILE> [OPTIONS]
```

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--format` | `-f` | Input format | Auto-detect from extension |

Examples:

```sh
haystack validate data/entities.zinc
haystack validate data/entities.json --format json
```

### `info`

Show information about the Haystack standard library.

```sh
haystack info [OPTIONS]
```

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--def` | `-d` | Show info about a specific def | Show general stats |

Examples:

```sh
haystack info
haystack info --def ahu
haystack info --def site
```

### `libs`

List loaded libraries in the standard namespace.

```sh
haystack libs
```

Output: table of library name, version, and number of defs.

### `specs`

List loaded Xeto specs.

```sh
haystack specs [OPTIONS]
```

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--lib` | `-l` | Filter specs by library name | Show all |

Examples:

```sh
haystack specs
haystack specs --lib ph
```

### `client`

Query a remote Haystack server. All subcommands require `--url`, `-U` (username), and `-P` (password).

Common flags for all `client` subcommands:

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--url` | | Server API URL (e.g., `http://localhost:8080/api`) | Required |
| `--username` | `-U` | Username | Required |
| `--password` | `-P` | Password | Required |
| `--format` | `-f` | Output format: `zinc`, `json`, `trio`, `json3` | `zinc` |

#### `client about`

Get server information.

```sh
haystack client about --url http://localhost:8080/api -U admin -P s3cret
```

#### `client read`

Read entities by filter.

```sh
haystack client read <FILTER> [OPTIONS]
```

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--limit` | `-l` | Maximum rows to return | No limit |

```sh
haystack client read "site" --url http://localhost:8080/api -U admin -P s3cret
haystack client read "point and temp" --limit 10 --url http://localhost:8080/api -U admin -P s3cret
```

#### `client nav`

Navigate the entity tree.

```sh
haystack client nav [OPTIONS]
```

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--nav_id` | | Navigation ID (omit for root) | Root |

```sh
haystack client nav --url http://localhost:8080/api -U admin -P s3cret
haystack client nav --nav_id @site-1 --url http://localhost:8080/api -U admin -P s3cret
```

#### `client hisread`

Read historical data for a point.

```sh
haystack client hisread <ID> [OPTIONS]
```

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--range` | `-r` | Date range | Required |

```sh
haystack client hisread @point-1 --range today --url http://localhost:8080/api -U admin -P s3cret
haystack client hisread @point-1 --range "2024-01-01,2024-01-31" --url http://localhost:8080/api -U admin -P s3cret
```

#### `client ops`

List supported server operations.

```sh
haystack client ops --url http://localhost:8080/api -U admin -P s3cret
```

#### `client libs`

List libraries from a remote server.

```sh
haystack client libs --url http://localhost:8080/api -U admin -P s3cret
```

#### `client specs`

List specs from a remote server.

```sh
haystack client specs [OPTIONS]
```

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--lib` | `-l` | Filter by library name | Show all |

```sh
haystack client specs --url http://localhost:8080/api -U admin -P s3cret
haystack client specs --lib ph --url http://localhost:8080/api -U admin -P s3cret
```

### `user`

Manage users in a TOML credentials file.

#### `user add`

Add a new user.

```sh
haystack user add <USERNAME> [OPTIONS]
```

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `--file` | `-f` | Path to users TOML file | Required |
| `--password` | `-p` | Password | Required |
| `--permissions` | `-r` | Comma-delimited permissions | `read` |

```sh
haystack user add admin --file users.toml --password s3cret --permissions read,write,admin
haystack user add viewer --file users.toml --password viewpass
```

#### `user delete`

Delete a user.

```sh
haystack user delete <USERNAME> --file users.toml
```

#### `user list`

List all users and their permissions.

```sh
haystack user list --file users.toml
```

#### `user passwd`

Update a user's password.

```sh
haystack user passwd <USERNAME> --file users.toml --password newpass
```

## Format Auto-Detection

When `--format` is omitted, the CLI detects format from the file extension:

| Extension | Format |
|-----------|--------|
| `.zinc` | `text/zinc` |
| `.trio` | `text/trio` |
| `.json` | `application/json` |

For explicit control, use the `--format` flag with: `zinc`, `trio`, `json`, `json3`.
