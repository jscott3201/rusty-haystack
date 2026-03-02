# Python Bindings

The `rusty_haystack` Python package provides access to the Haystack core library via PyO3.

## Installation

Requires Python 3.8+ and [maturin](https://www.maturin.rs/):

```sh
python -m venv .venv
source .venv/bin/activate
pip install maturin
cd rusty-haystack
maturin develop --release
```

## Quick Start

```python
import rusty_haystack as rh

# Create an entity
entity = rh.HDict({
    "id": rh.Ref("site-1", "Demo Site"),
    "site": rh.Marker(),
    "dis": "Demo Site",
    "area": rh.Number(5000, "ft\u00b2"),
    "geoCoord": rh.Coord(40.7128, -74.0060),
})

# Build a graph
graph = rh.EntityGraph()
graph.add(entity)

# Query
results = graph.read("site")
for row in results:
    print(row["dis"])

# Encode/decode
zinc_str = rh.encode_grid("text/zinc", results)
decoded = rh.decode_grid("text/zinc", zinc_str)

# Validate
ns = rh.DefNamespace.load_standard()
issues = ns.validate_entity(entity)
```

## Kind Types

All kind types are immutable (frozen).

### Marker, NA, Remove

```python
m = rh.Marker()     # presence tag
na = rh.NA()         # not available
rm = rh.Remove()     # tag removal sentinel
```

### Number

```python
n = rh.Number(72.5, "\u00b0F")  # with unit
n = rh.Number(42)               # unitless

n.val    # -> 72.5 (float)
n.unit   # -> "\u00b0F" or None
float(n) # -> 72.5
int(n)   # -> 72
```

Supports `==`, `!=`, `<`, `<=`, `>`, `>=` (requires matching units for ordering).

### Ref

```python
r = rh.Ref("site-1")                # without display name
r = rh.Ref("site-1", "Demo Site")   # with display name

r.val   # -> "site-1"
r.dis   # -> "Demo Site" or None
```

Equality compares by `val` only; `dis` is ignored.

### Uri, Symbol, XStr

```python
u = rh.Uri("http://example.com")    # u.val -> "http://example.com"
s = rh.Symbol("hot-water")          # s.val -> "hot-water"
x = rh.XStr("Bin", "base64data")    # x.type_name -> "Bin", x.val -> "base64data"
```

### Coord

```python
c = rh.Coord(40.7128, -74.0060)
c.lat  # -> 40.7128
c.lng  # -> -74.006
```

### HDateTime

```python
dt = rh.HDateTime(2024, 1, 15, 10, 30, 0, -18000, "New_York")

dt.tz_name  # -> "New_York"
dt.dt()     # -> datetime.datetime with timezone
```

Supports full ordering (`<`, `<=`, `>`, `>=`).

## Data Structures

### HDict

Mutable dictionary mapping tag names to values. Supports the full Python dict protocol.

```python
d = rh.HDict({"site": rh.Marker(), "dis": "My Site"})

# Dict protocol
d["dis"]                    # -> "My Site"
d["area"] = rh.Number(100)  # set
del d["area"]               # delete
"site" in d                 # -> True
len(d)                      # -> 2
list(d)                     # -> ["site", "dis"]

# Methods
d.has("site")       # -> True
d.missing("area")   # -> True
d.get("dis")        # -> "My Site"
d.get("missing")    # -> None
d.id()              # -> Ref or None
d.dis()             # -> display string or None
d.is_empty()        # -> False
d.set("tag", val)
d.merge(other_dict)
d.keys()            # -> ["site", "dis"]
d.values()          # -> [Marker(), "My Site"]
d.items()           # -> [("site", Marker()), ("dis", "My Site")]
```

### HGrid

Immutable tabular data. Supports indexing and iteration.

```python
grid = rh.decode_grid("text/zinc", zinc_str)

grid.is_empty()   # -> bool
grid.is_err()     # -> bool
grid.num_cols()   # -> int
grid.col_names()  # -> ["id", "dis", "site"]
grid.col("dis")   # -> HCol or None
grid.meta()       # -> HDict (grid metadata)
len(grid)         # -> number of rows
grid[0]           # -> HDict (first row)

for row in grid:
    print(row["dis"])
```

### HList

Immutable list of values.

```python
lst = rh.HList([rh.Number(1), rh.Number(2), rh.Number(3)])

len(lst)     # -> 3
lst[0]       # -> Number(1)
lst[-1]      # -> Number(3)
lst.is_empty()

for item in lst:
    print(item)
```

### HCol

Grid column descriptor.

```python
col = rh.HCol("temperature")
col.name  # -> "temperature"
```

## Codec Functions

Supported MIME types: `"text/zinc"`, `"text/trio"`, `"application/json"`, `"application/json;v=3"`, `"text/csv"`.

```python
# Grid encode/decode
zinc_str = rh.encode_grid("text/zinc", grid)
grid = rh.decode_grid("text/zinc", zinc_str)

json_str = rh.encode_grid("application/json", grid)
grid = rh.decode_grid("application/json", json_str)

# Scalar encode/decode
s = rh.encode_scalar("text/zinc", rh.Number(72.5, "\u00b0F"))
v = rh.decode_scalar("text/zinc", s)
```

Raises `KeyError` for unknown codecs, `ValueError` for encoding/decoding errors.

## Filter Functions

```python
# Parse a filter (returns AST debug string)
ast = rh.parse_filter("site and area > 1000")

# Evaluate a filter against an entity
entity = rh.HDict({"site": rh.Marker(), "area": rh.Number(5000)})
rh.matches_filter("site and area > 1000", entity)  # -> True
rh.matches_filter("equip", entity)                  # -> False
```

Raises `ValueError` for invalid filter expressions.

## EntityGraph

In-memory entity graph with CRUD, filtering, and ref traversal.

```python
graph = rh.EntityGraph()

# Add entities (must have "id" Ref tag)
site = rh.HDict({
    "id": rh.Ref("site-1"),
    "site": rh.Marker(),
    "dis": "Demo Site",
})
graph.add(site)  # returns "site-1"

equip = rh.HDict({
    "id": rh.Ref("ahu-1"),
    "equip": rh.Marker(),
    "ahu": rh.Marker(),
    "siteRef": rh.Ref("site-1"),
})
graph.add(equip)

# Get entity
entity = graph.get("site-1")  # -> HDict or None

# Update
changes = rh.HDict({"dis": "Updated Site"})
graph.update("site-1", changes)

# Remove
removed = graph.remove("ahu-1")  # -> HDict

# Query
results = graph.read("site")           # -> HGrid
results = graph.read("equip", limit=10)

# Ref traversal
graph.refs_from("ahu-1")                     # -> ["site-1"]
graph.refs_from("ahu-1", ref_type="siteRef") # -> ["site-1"]
graph.refs_to("site-1")                      # -> ["ahu-1"]

# Export
grid = graph.to_grid()              # all entities
grid = graph.to_grid("site")        # filtered

# Metadata
len(graph)              # number of entities
"site-1" in graph       # containment check
graph.version()         # monotonic version counter
```

## Ontology & Xeto

### DefNamespace

```python
ns = rh.DefNamespace.load_standard()  # loads ph, phScience, phIoT, phIct

# Type checking
ns.is_a("ahu", "equip")      # -> True (nominal subtype)
ns.contains("site")          # -> True
len(ns)                       # number of defs

# Entity validation
ns.fits(entity, "site")      # -> True (structural fitting)
issues = ns.validate_entity(entity)  # -> list of issue strings

# Type relationships
ns.subtypes("equip")         # -> ["ahu", "vav", ...]
ns.supertypes("ahu")         # -> ["equip", "entity", ...]

# Xeto library management
spec_names = ns.load_xeto(source_text, "myLib")
ns.unload_lib("myLib")
xeto_source = ns.export_lib_xeto("myLib")

# Spec lookup
ns.specs()                   # -> list of all Spec objects
ns.specs(lib="ph")           # -> specs from the ph library
spec = ns.get_spec("ph::Ahu")
```

### Spec

```python
spec.qname        # -> "ph::Ahu"
spec.name          # -> "Ahu"
spec.lib           # -> "ph"
spec.base          # -> "ph::Equip" or None
spec.doc           # -> docstring
spec.is_abstract   # -> bool
spec.slots         # -> list of Slot

spec.markers()            # -> ["ahu", "equip"]
spec.mandatory_markers()  # -> ["ahu", "equip"]
```

### Slot

```python
slot.name       # -> "ahu"
slot.type_ref   # -> "Marker" or None
slot.is_marker  # -> True
slot.is_query   # -> False
slot.is_maybe   # -> False
```

## SharedGraph

Thread-safe wrapper around `EntityGraph` using `Arc<RwLock>`. Safe to share across threads.

```python
graph = rh.SharedGraph()                    # empty graph
graph = rh.SharedGraph(entity_graph)        # wrap existing EntityGraph

# CRUD (same interface as EntityGraph)
ref_val = graph.add(entity)                 # -> "site-1"
entity = graph.get("site-1")               # -> HDict or None
graph.update("site-1", rh.HDict({"dis": "New Name"}))
removed = graph.remove("site-1")           # -> HDict

# Query
results = graph.read("site and area > 1000")       # -> HGrid
results = graph.read("equip", limit=50)

# Ref traversal
graph.refs_from("ahu-1")                   # -> ["site-1"]
graph.refs_from("ahu-1", ref_type="siteRef")
graph.refs_to("site-1")                    # -> ["ahu-1", "ahu-2"]

# Metadata
graph.all()         # -> HGrid (all entities)
graph.is_empty()    # -> bool
graph.contains("site-1")  # -> bool
graph.version()     # -> int

# Change tracking
diffs = graph.changes_since(old_version)   # -> list[GraphDiff]

# Validation
issues = graph.validate(namespace)         # -> list[str]
fitting = graph.entities_fitting(namespace, "ahu")  # -> list[HDict]
```

### GraphDiff

Represents a change to the graph (add, update, or remove).

```python
diff.op          # -> DiffOp.Add, DiffOp.Update, or DiffOp.Remove
diff.ref_val     # -> "site-1"
diff.old         # -> HDict or None (previous state, for Update/Remove)
diff.new         # -> HDict or None (new state, for Add/Update)
```

## Filter Builder

Programmatic filter construction using the `Filter` class.

```python
# Parse from string
f = rh.Filter.parse("site and area > 1000")

# Builder methods
f = rh.Filter.has("site")
f = rh.Filter.missing("deprecated")
f = rh.Filter.cmp(rh.Path("area"), ">", rh.Number(1000))

# Combine filters
combined = f.and_filter(rh.Filter.has("geoCity"))
combined = f.or_filter(rh.Filter.has("campus"))

# Evaluate against an entity
f.matches(entity)   # -> bool

# String representation
str(f)              # -> "site and area > 1000"
```

## HTTP Client

Synchronous HTTP client for communicating with Haystack servers. All methods block the calling thread; the GIL is released during network I/O.

```python
# Connect with SCRAM SHA-256 authentication
client = rh.HaystackClient.connect(
    "http://localhost:8080/api",
    "admin",
    "s3cret",
)

# Server info
about = client.about()       # -> HGrid
ops = client.ops()           # -> HGrid
formats = client.formats()   # -> HGrid
libs = client.libs()         # -> HGrid

# Read entities
sites = client.read("site")                     # -> HGrid
sites = client.read("site and area > 1000", limit=10)
entities = client.read_by_ids(["site-1", "ahu-1"])

# Navigation
roots = client.nav()                    # -> HGrid (root nodes)
children = client.nav(nav_id="site-1")  # -> HGrid (children)

# Definitions
all_defs = client.defs()
equip_defs = client.defs(filter="equip")

# History
his = client.his_read("point-1", "today")
his = client.his_read("point-1", "2024-01-01,2024-02-01")
client.his_write("point-1", [
    rh.HDict({"ts": rh.HDateTime(2024, 1, 15, 10, 0, 0, 0, "UTC"), "val": rh.Number(72)}),
])

# Point writes (priority levels 1-17)
client.point_write("point-1", level=16, val=rh.Number(72))

# Actions
result = client.invoke_action("equip-1", "reboot", rh.HDict())

# Watches
sub = client.watch_sub(["point-1", "point-2"], lease="1min")
changes = client.watch_poll("watch-id")
client.watch_unsub("watch-id", ["point-1"])

# Specs and libraries
specs = client.specs()
spec = client.spec("ph::Ahu")
client.load_lib("myLib", xeto_source)
client.unload_lib("myLib")
exported = client.export_lib("myLib")

# Validation
results = client.validate([entity1, entity2])

# Generic call
result = client.call("customOp", request_grid)

# Clean up
client.close()
```

### WebSocket Client

WebSocket transport with the same API. Uses Zinc encoding and JSON message envelope.

```python
client = rh.HaystackClient.connect_ws(
    "http://localhost:8080/api",       # HTTP URL (for SCRAM auth)
    "ws://localhost:8080/api/ws",      # WebSocket URL
    "admin",
    "s3cret",
)

# Same API as HTTP client
sites = client.read("site")
client.close()
```

### mTLS Client

Connect with mutual TLS client certificates.

```python
client = rh.HaystackClient.connect_tls(
    "https://secure-server:8443/api",
    "admin",
    "s3cret",
    cert_path="/path/to/client.pem",
    key_path="/path/to/client-key.pem",
    ca_path="/path/to/ca.pem",        # optional
)
```

## Embedded Server

Run a Haystack server directly from Python. Useful for testing, prototyping, or embedding in larger applications.

```python
import rusty_haystack as rh

# Build the graph
graph = rh.SharedGraph()
graph.add(rh.HDict({
    "id": rh.Ref("site-1"),
    "site": rh.Marker(),
    "dis": "Demo Site",
}))

# Configure and start (blocks the thread)
server = rh.HaystackServer(graph)
server.port(8080)
server.host("0.0.0.0")
server.run()  # blocks until shutdown
```

### Background Server

Start the server in a background thread for non-blocking usage (e.g., in Jupyter notebooks).

```python
server = rh.HaystackServer(graph)
server.port(8080)
server.run_background()

# Server is running — use client to interact
client = rh.HaystackClient.connect("http://localhost:8080/api", "admin", "pw")
print(client.about())

# Check for startup errors
err = server.bg_error()  # -> str or None
```

### Server with Authentication

```python
auth = rh.AuthManager.from_toml("users.toml")
# Or: auth = rh.AuthManager.from_toml_str(toml_string)

server = rh.HaystackServer(graph)
server.with_auth(auth)
server.port(8080)
server.run()
```

### Server with Ontology

```python
ns = rh.DefNamespace.load_standard()

server = rh.HaystackServer(graph)
server.with_namespace(ns)
server.port(8080)
server.run()
```

## Federation

Aggregate entities from multiple remote Haystack servers.

```python
# Load from TOML config
fed = rh.Federation.from_toml("federation.toml")

# Or build programmatically
fed = rh.Federation()
fed.add(rh.ConnectorConfig(
    name="Building A",
    url="http://building-a:8080/api",
    username="federation",
    password="s3cret",
    id_prefix="bldg-a-",
    sync_interval_secs=30,
))

# Attach to server
server = rh.HaystackServer(graph)
server.with_federation(fed)
server.port(8080)
server.run()
```

### ConnectorConfig Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | `str` | Yes | Display name |
| `url` | `str` | Yes | Remote API base URL |
| `username` | `str` | Yes | SCRAM auth username |
| `password` | `str` | Yes | SCRAM auth password |
| `id_prefix` | `str` | No | Ref value prefix for namespacing |
| `ws_url` | `str` | No | WebSocket URL override |
| `sync_interval_secs` | `int` | No | Background sync interval (default: 60) |
| `client_cert` | `str` | No | Path to mTLS client certificate |
| `client_key` | `str` | No | Path to mTLS client private key |
| `ca_cert` | `str` | No | Path to CA certificate |

### Federation Status

```python
# Sync all connectors and get results
results = fed.sync_all()  # -> list[tuple[str, str]] (name, status)

# Query federated cache
cached = fed.filter_cached("site")        # -> HGrid
cached = fed.filter_cached("equip", limit=100)

# Metadata
fed.status()             # -> list[HDict] (per-connector status)
fed.connector_count()    # -> int
fed.is_enabled()         # -> bool
```

## SCRAM Authentication

Low-level SCRAM SHA-256 utilities for custom auth flows.

```python
# Hash a password for storage
hashed = rh.hash_password("s3cret")

# Derive SCRAM credentials
creds = rh.derive_credentials("s3cret", salt_bytes, iterations)
```

## Type Conversion

| Python | Haystack |
|--------|----------|
| `None` | Null |
| `bool` | Bool |
| `int`, `float` | Number (unitless) |
| `str` | Str |
| `datetime.date` | Date |
| `datetime.time` | Time |
| `Marker` | Marker |
| `NA` | NA |
| `Remove` | Remove |
| `Number` | Number |
| `Ref` | Ref |
| `Uri` | Uri |
| `Symbol` | Symbol |
| `XStr` | XStr |
| `Coord` | Coord |
| `HDateTime` | DateTime |
| `HDict` | Dict |
| `HGrid` | Grid |
| `HList` | List |

Note: Python `datetime.datetime` is not supported directly. Use `HDateTime` instead.

## Version

```python
rh.__version__  # -> "0.1.0"
```
