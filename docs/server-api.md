# Server API Reference

The Haystack server exposes all endpoints under `/api`. All POST endpoints accept and return grids in the negotiated wire format.

## Content Negotiation

- **Request format**: determined by `Content-Type` header
- **Response format**: determined by `Accept` header
- **Default**: `text/zinc` for both
- **Supported formats**:

| MIME Type | Format |
|-----------|--------|
| `text/zinc` | Zinc (default) |
| `application/json` | JSON Haystack v4 |
| `text/trio` | Trio |
| `application/json;v=3` | JSON Haystack v3 |
| `text/csv` | CSV (response only) |

The `Accept` header supports quality factors (e.g., `application/json;q=0.9, text/zinc;q=1.0`).

## Limits

| Limit | Value |
|-------|-------|
| Request body size | 2 MB |
| Max watches | 100 per server |
| Max IDs per watch | 10,000 |
| Max history items | 1,000,000 per point |
| Handshake TTL | 60 seconds |
| Token TTL | 3,600 seconds (default) |

## Error Format

Errors are returned as grids with an `err` marker and `dis` string in the grid metadata:

```zinc
ver:"3.0" err dis:"Entity not found: @missing"
empty
```

HTTP status codes: 200 (success), 400 (bad request), 401 (unauthorized), 403 (forbidden), 404 (not found), 500 (internal error).

## Authentication

See the [SCRAM Authentication](#scram-authentication) section below. When auth is disabled (no `--users` flag), all endpoints are accessible without credentials.

## Endpoints

### Information (No Auth Required)

#### GET `/api/about`

Server information. Also handles the SCRAM handshake (see below).

**Response grid columns**: `haystackVersion`, `serverName`, `serverVersion`, `tz`, `serverTime`, `serverBootTime`, `productName`, `productVersion`, `productUri`

#### GET `/api/ops`

List supported operations.

**Response grid columns**: `name` (Str), `summary` (Str)

#### GET `/api/formats`

List supported wire formats.

**Response grid columns**: `mime` (Str), `receive` (Marker), `send` (Marker)

### Read Operations (read permission)

#### POST `/api/read`

Read entities by filter or by ID.

**Request grid** — one of:
- Filter mode: `filter` column (Str), optional `limit` column (Number)
- ID mode: `id` column (Ref) — one row per entity

Includes results from federated remote connectors.

**Response**: grid with all matched entities.

#### POST `/api/nav`

Navigate the entity tree.

**Request grid**: optional `navId` column (Str or Ref)
- Omitted: returns top-level sites
- Site ref: returns children (equips/spaces with `siteRef`)
- Equip ref: returns children (points with `equipRef`)

**Response grid columns**: `id` (Ref), `dis` (Str), `navId` (Str)

#### POST `/api/defs`

Query the definition namespace.

**Request grid**: optional `filter` column (Str) for symbol substring filtering

**Response grid columns**: `def` (Symbol), `lib` (Symbol), `doc` (Str) — sorted by symbol name

#### POST `/api/libs`

List loaded libraries.

**Response grid columns**: `name` (Str), `version` (Str) — sorted by name

#### POST `/api/specs`

List Xeto spec definitions.

**Request grid**: optional `lib` column (Str) to filter by library

**Response grid columns**: `qname` (Str), `name` (Str), `lib` (Str), `base` (Str), `doc` (Str), `abstract` (Marker)

#### POST `/api/spec`

Get a single spec by qualified name.

**Request grid**: `qname` column (Str)

**Response grid columns**: `qname`, `name`, `lib`, `base`, `doc`, `abstract` (Marker), `slots` (Str, comma-separated)

#### POST `/api/hisRead`

Read historical time-series data.

**Request grid**: `id` (Ref), `range` (Str)

Range formats:
- `"today"` / `"yesterday"` — local date ranges
- `"YYYY-MM-DD"` — single date (midnight to midnight)
- `"YYYY-MM-DD,YYYY-MM-DD"` — start (inclusive) to end (exclusive midnight)

**Response grid columns**: `ts` (DateTime), `val` (varies)

#### POST `/api/export`

Bulk export all entities.

**Response**: grid with all entities and their tags.

#### GET `/api/rdf/turtle`

Export all entities as RDF Turtle. Returns `text/turtle` content type.

#### GET `/api/rdf/jsonld`

Export all entities as JSON-LD. Returns `application/ld+json` content type.

#### GET `/api/federation/status`

Federation connector status.

**Response grid columns**: `name` (Str), `entityCount` (Number)

### Watch Operations (read permission)

#### POST `/api/watchSub`

Subscribe to entity changes.

**Request grid**: `id` column (Ref) — entities to watch. Optional `watchId` in grid meta to add to existing watch.

**Response**: grid with current state of watched entities. Grid meta contains `watchId` (Str).

#### POST `/api/watchPoll`

Poll a watch for changes since last poll.

**Request grid meta**: `watchId` (Str)

**Response**: grid with changed entities.

#### POST `/api/watchUnsub`

Unsubscribe from a watch.

**Request grid meta**: `watchId` (Str). Optional `id` column (Ref) to remove specific IDs; otherwise removes entire watch.

### Write Operations (write permission)

#### POST `/api/pointWrite`

Write a value to a writable point.

**Request grid**: `id` (Ref), `level` (Number, 1-17, default 17), `val` (varies). Target entity must have `writable` marker.

#### POST `/api/hisWrite`

Write historical data.

**Request grid meta**: `id` (Ref). Rows: `ts` (DateTime), `val` (varies).

#### POST `/api/invokeAction`

Invoke an action on an entity.

**Request grid**: `id` (Ref), `action` (Str), plus additional columns for action arguments.

**Response**: grid returned by the action handler.

#### POST `/api/import`

Bulk import entities. Updates existing entities (by ID), adds new ones.

**Request grid**: rows with `id` (Ref) and entity tags.

**Response grid**: `count` (Number) of imported entities.

#### POST `/api/loadLib`

Load a Xeto library from source text.

**Request grid**: `name` (Str), `source` (Str)

**Response grid**: `loaded` (Str), `specs` (Str, comma-separated)

#### POST `/api/unloadLib`

Unload a library.

**Request grid**: `name` (Str)

**Response grid**: `unloaded` (Str)

#### POST `/api/exportLib`

Export a library to Xeto source text.

**Request grid**: `name` (Str)

**Response grid**: `name` (Str), `source` (Str)

#### POST `/api/validate`

Validate entities against the ontology.

**Request grid**: rows are entities to validate.

**Response grid columns**: `entity` (Str), `issueType` (Str), `detail` (Str) — one row per issue.

#### POST `/api/federation/sync`

Synchronize all federated remote connectors.

**Response grid columns**: `name` (Str), `result` (Str), `ok` (Bool)

### Session

#### POST `/api/close`

Revokes the current bearer token (logout). Requires read permission.

### Admin Operations (admin permission)

#### GET `/api/system/status`

Server status.

**Response grid columns**: `uptime` (Number, seconds), `entityCount` (Number), `watchCount` (Number)

#### POST `/api/system/backup`

Export all entities as JSON backup. Always returns `application/json`.

#### POST `/api/system/restore`

Import entities from a JSON backup.

**Response grid**: `count` (Number) of restored entities.

## WebSocket

### Endpoint

`GET /api/ws` — upgrades to WebSocket connection. Requires a valid bearer token if auth is enabled.

### Message Format

Request:
```json
{
  "op": "watchSub",
  "reqId": "1",
  "watchDis": "My Watch",
  "ids": ["@site-1", "@equip-1"]
}
```

Response:
```json
{
  "reqId": "1",
  "watchId": "abc-123",
  "rows": [
    {"id": "r:site-1", "dis": "s:Demo Site", "site": "m:"}
  ]
}
```

Supported operations: `watchSub`, `watchPoll`, `watchUnsub`.

Fields:
- `op` (required): operation name
- `reqId` (optional): request ID echoed in response
- `watchDis` (optional): display name for new watch
- `watchId` (optional): existing watch ID
- `ids` (optional): array of entity ref strings (`@` prefix stripped automatically)

Error responses include an `error` field with the message.

## SCRAM Authentication

The server implements SCRAM SHA-256 via the `/api/about` endpoint.

### Phase 1: HELLO

```
GET /api/about
Authorization: HELLO username=<base64(username)>
```

Response: `401 Unauthorized`
```
WWW-Authenticate: SCRAM handshakeToken=<token> hash=SHA-256 data=<server_first_b64>
```

### Phase 2: SCRAM

```
GET /api/about
Authorization: SCRAM handshakeToken=<token> data=<client_final_b64>
```

Response: `200 OK`
```
Authentication-Info: authToken=<token> data=<server_final_b64>
```

The client should verify the server signature from `data` to prevent MITM attacks.

### Phase 3: Subsequent Requests

```
POST /api/read
Authorization: BEARER authToken=<token>
```

### Security Details

- PBKDF2 with 100,000 iterations
- Constant-time credential comparison (prevents timing attacks)
- Fake SCRAM challenge for unknown users (prevents username enumeration)
- Handshake timeout: 60 seconds
- Token lifetime: 3,600 seconds (configurable)

## Permission Model

| Permission | Endpoints |
|------------|-----------|
| (none) | `GET /api/about`, `GET /api/ops`, `GET /api/formats` |
| read | `read`, `nav`, `defs`, `libs`, `specs`, `spec`, `hisRead`, `export`, `watchSub`, `watchPoll`, `watchUnsub`, `close`, `rdf/*`, `federation/status` |
| write | `pointWrite`, `hisWrite`, `invokeAction`, `import`, `loadLib`, `unloadLib`, `exportLib`, `validate`, `federation/sync` |
| admin | `system/status`, `system/backup`, `system/restore` |
