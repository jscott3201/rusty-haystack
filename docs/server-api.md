# Server API Reference

The Haystack server (built on Axum) exposes all endpoints under `/api`. All POST endpoints accept and return grids in the negotiated wire format.

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
| hisWrite row limit | 100,000 rows per request |
| Max watches | 100 per server |
| Max IDs per watch | 10,000 |
| Max history items | 1,000,000 per point |
| Handshake TTL | 60 seconds |
| Token TTL | 3,600 seconds (default) |
| Parser nesting depth | 64 levels |
| Parser string size | 10 MB |
| Parser collection size | 1,000,000 elements |

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

**Request grid** -- one of:
- Filter mode: `filter` column (Str), optional `limit` column (Number)
- ID mode: `id` column (Ref) -- one row per entity

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

**Response grid columns**: `def` (Symbol), `lib` (Symbol), `doc` (Str) -- sorted by symbol name

#### POST `/api/libs`

List loaded libraries.

**Response grid columns**: `name` (Str), `version` (Str) -- sorted by name

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
- `"today"` / `"yesterday"` -- local date ranges
- `"YYYY-MM-DD"` -- single date (midnight to midnight)
- `"YYYY-MM-DD,YYYY-MM-DD"` -- start (inclusive) to end (exclusive midnight)

**Response grid columns**: `ts` (DateTime), `val` (varies)

#### POST `/api/export`

Bulk export all entities.

**Response**: grid with all entities and their tags.

### Watch Operations (read permission)

#### POST `/api/watchSub`

Subscribe to entity changes.

**Request grid**: `id` column (Ref) -- entities to watch. Optional `watchId` in grid meta to add to existing watch.

**Response**: grid with current state of watched entities. Grid meta contains `watchId` (Str).

WebSocket disconnect cleanup currently removes all watches owned by that username, including watches created through another connection or HTTP.

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

Row limit: 100,000 rows per request.

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

**Response grid columns**: `entity` (Str), `issueType` (Str), `detail` (Str) -- one row per issue.

### Session

#### POST `/api/close`

Revokes the current bearer token (logout). Requires read permission.

## WebSocket

### Endpoint

`GET /api/ws` -- upgrades to WebSocket connection. Requires a valid bearer token if auth is enabled.

### Message Format

Messages are uncompressed JSON text. Individual frames and complete reassembled
messages are each capped at 1 MiB. Binary application messages and compression
are unsupported; malformed envelopes close the connection. A nonempty string
`reqId` of at most 128 bytes is required on every request and echoed on responses.

```json
{"op":"watchSub","reqId":"1","ids":["@site-1","@equip-1"]}
```

```json
{"reqId":"1","watchId":"abc-123","rows":[{"id":"r:site-1","dis":"s:Demo Site","site":"m:"}]}
```

Rows contain Haystack JSON v3 typed values. Supported operations and arguments:

| Operation | Arguments | Successful response |
| --- | --- | --- |
| `watchSub` | `ids`: 1–1,000 entity refs; no `watchId` | New `watchId` and current rows |
| `watchPoll` | Nonempty `watchId`; no `ids` | Same `watchId` and changed rows |
| `watchUnsub` | Nonempty `watchId`; optional `ids` | Empty rows; same `watchId` when removing selected IDs, omitted when removing the entire watch |

Each entity ref is nonempty after stripping one optional leading `@` and at most
1,024 bytes. Watch IDs are at most 128 bytes. Empty or absent `ids` for
`watchUnsub` removes the entire watch. A nonempty list removes only those IDs,
leaving an empty watch alive when all IDs have been removed. Leases, `watchDis`,
and adding IDs to an existing watch are outside this WebSocket profile.

An unsupported operation or invalid operation arguments receive a correlated
error, for example `{"reqId":"1","error":"unsupported watch arguments"}`.
Missing/invalid request IDs, unexpected fields, invalid JSON, and incompatible
message types terminate the connection instead of creating an uncorrelated reply.

Unsolicited notifications are separate from responses:

```json
{"type":"push","watchId":"abc-123","rows":[{"id":"r:site-1","dis":"s:Updated Site"}]}
```

The outbound queue holds 64 messages. Queue overflow, oversized output, and writer
failure close the connection rather than silently losing a response or push.
The socket owns its writer task and joins or aborts it on shutdown. The server
sends a ping every 30 seconds and closes after a separate 10-second pong deadline.
Clients must re-establish their watches after loss of a connection.

Watch ownership and push routing remain username-scoped. Disconnecting one socket
removes all watches owned by that username, including watches used by other
connections or HTTP. This behavior does not provide connection-level isolation.

## SCRAM Authentication

The server implements the [published Haystack SCRAM SHA-256 exchange](https://project-haystack.org/doc/docHaystack/Auth) through three GETs to `/api/about`. Outer username/data fields use unpadded base64url; inner salt, proof, and verifier use standard padded Base64. Stored password hash encodings remain unchanged.

1. Send `Authorization: HELLO username=<base64url(username)>`. The 401 response contains `WWW-Authenticate: SCRAM hash=SHA-256, handshakeToken=<first-token>` and no data.
2. Send `Authorization: SCRAM handshakeToken=<first-token>, data=<client-first>`. The client-first decoded username must match HELLO. The 401 response contains `WWW-Authenticate: SCRAM hash=SHA-256, handshakeToken=<next-token>, data=<server-first>`.
3. Send `Authorization: SCRAM handshakeToken=<next-token>, data=<client-final>`. Successful proof verification and registered-user admission return 200 with `Authentication-Info: authToken=<bearer>, hash=SHA-256, data=<server-final>`. The client must verify the server signature before using the bearer.

The server always issues a handshake token, rotates it after client-first, and requires the most recently issued value. Each token is consumed once; concurrent or replayed final proofs cannot issue another bearer. Subsequent requests use `Authorization: BEARER authToken=<bearer>`.

Malformed authentication, wrong-stage messages, expired handshakes, wrong identities and invalid credentials return 403 without echoing submitted fields. Missing authentication prompts with 401. Invalid or expired bearer tokens return 401. Admission capacity exhaustion returns 503.

### Security Details

- New stored credentials use PBKDF2-HMAC-SHA-256 with 100,000 iterations; loaded credentials require valid key lengths, nonempty bounded salt, and 1–1,000,000 iterations.
- Proofs and server signatures use constant-time equality. Username normalization is not introduced.
- Unknown usernames receive secret-derived plausible challenges without a password derivation on the request thread. Even a correct decoy proof cannot issue a bearer. This reduces straightforward enumeration; it does not claim complete timing or identity-provider qualification.
- Default capacity is 1,024 in-flight handshakes and 4,096 active bearer tokens, configurable with `AuthLimits`. Admission, cleanup, and transitions share one lock.
- The 60-second handshake lifetime starts at HELLO and is preserved through rotation. Bearers expire after 3,600 seconds by default, configurable with `with_token_ttl`.
- Every auth-state operation sweeps expired entries before admission or lookup, including when abandoned clients never return. State stays bounded even without a background timer. These controls do not replace deployment rate limits.
- Auth headers are limited to 8 KiB, decoded data to 4 KiB, usernames and combined nonces to 1 KiB. Malformed encodings, duplicate fields, nonce mismatches, and incorrect proof lengths are rejected.

## Permission Model

| Permission | Endpoints |
|------------|-----------|
| (none) | `GET /api/about`, `GET /api/ops`, `GET /api/formats` |
| read | `read`, `nav`, `defs`, `libs`, `specs`, `spec`, `hisRead`, `export`, `watchSub`, `watchPoll`, `watchUnsub`, `close` |
| write | `pointWrite`, `hisWrite`, `invokeAction`, `import`, `loadLib`, `unloadLib`, `exportLib`, `validate` |
| admin | (reserved for future use) |
