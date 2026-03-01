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
