# Federation Demo

A 5-container Docker Compose setup demonstrating Haystack federation. One **lead** node aggregates entities from four **building** nodes over an internal Docker network.

## Architecture

```
                  ┌──────────────┐
  host:8080 ────▶ │     lead     │
                  │  (36 local + │
                  │  144 federated)│
                  └──┬──┬──┬──┬──┘
                     │  │  │  │   internal "haystack" network
              ┌──────┘  │  │  └──────┐
              ▼         ▼  ▼         ▼
          ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐
          │ node-1 │ │ node-2 │ │ node-3 │ │ node-4 │
          │ Bldg A │ │ Bldg B │ │ Bldg C │ │ Bldg D │
          │ 36 ent │ │ 36 ent │ │ 36 ent │ │ 36 ent │
          └────────┘ └────────┘ └────────┘ └────────┘
```

Each node runs `--demo` mode which loads 36 built-in entities (1 site, 3 floors, 2 AHUs, 6 VAVs, 24 points). The lead node federates all four, prefixing their IDs (`bldg-a-`, `bldg-b-`, `bldg-c-`, `bldg-d-`) so they coexist in a single namespace.

## Credentials

| User | Password | Permissions |
|------|----------|-------------|
| `admin` | `admin` | read, write, admin |
| `federation` | `federation` | read |

The lead node uses the `federation` user to sync from each building node.

## Running

```sh
cd demo
docker compose up --build
```

Wait for the build and startup. The lead node depends on all four building nodes, so it starts last. After ~15 seconds the first sync cycle completes.

## Trying It Out

All examples use the lead node at `localhost:8080`.

### Check federation status

```sh
curl -s http://localhost:8080/api/federation/status \
  -H "Accept: application/json" | jq
```

You should see 4 connectors, each with `entityCount: 36` and `connected: true`.

### Read all entities (local + federated)

```sh
curl -s -X POST http://localhost:8080/api/read \
  -H "Content-Type: text/zinc" \
  -H "Accept: application/json" \
  -d 'ver:"3.0"
filter
"*"'
```

Returns ~180 entities: 36 local + 144 federated (36 per building).

### Read a specific federated entity

```sh
curl -s -X POST http://localhost:8080/api/read \
  -H "Content-Type: text/zinc" \
  -H "Accept: application/json" \
  -d 'ver:"3.0"
id
@bldg-a-demo-site'
```

Returns Building A's site entity with prefixed refs.

### Filter across all buildings

```sh
curl -s -X POST http://localhost:8080/api/read \
  -H "Content-Type: text/zinc" \
  -H "Accept: application/json" \
  -d 'ver:"3.0"
filter
"point and temp"'
```

Returns all temperature points from the local site plus all four buildings.

### Navigate the tree

```sh
# Top-level sites (local + federated)
curl -s -X POST http://localhost:8080/api/nav \
  -H "Content-Type: text/zinc" \
  -H "Accept: application/json" \
  -d 'ver:"3.0"
empty'
```

### Trigger manual sync

```sh
# Sync all connectors
curl -s -X POST http://localhost:8080/api/federation/sync \
  -H "Accept: application/json"

# Sync a single connector
curl -s -X POST http://localhost:8080/api/federation/sync/Building%20A \
  -H "Accept: application/json"
```

## Stopping

```sh
docker compose down
```

## Notes

- The sync interval is set to 15 seconds for demo responsiveness.
- All nodes use the same demo data, so federated entities differ only by their ID prefix.
- The internal `haystack` bridge network keeps all inter-node traffic off the host.
- Only the lead node's port 8080 is exposed to the host.
