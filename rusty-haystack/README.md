# rusty-haystack

Fast Python bindings for [Project Haystack](https://project-haystack.org) powered by Rust.

Provides types, codecs (Zinc/Trio/JSON/JSON v3/CSV), filters, an in-memory entity graph, ontology validation, and Xeto support — all backed by a native Rust core for speed and correctness.

## Installation

```sh
pip install rusty-haystack
```

Requires Python 3.11+. Pre-built wheels are available for Linux and macOS (x86_64 and aarch64).

## Quick Start

```python
import rusty_haystack as rh

# Create an entity
entity = rh.HDict({
    "id": rh.Ref("site-1", "Demo Site"),
    "site": rh.Marker(),
    "dis": "Demo Site",
    "area": rh.Number(5000, "ft²"),
    "geoCoord": rh.Coord(40.7128, -74.0060),
})

# Build a graph and query it
graph = rh.EntityGraph()
graph.add(entity)
results = graph.read("site")

# Encode/decode across formats
zinc_str = rh.encode_grid("text/zinc", results)
json_str = rh.encode_grid("application/json", results)
decoded = rh.decode_grid("text/zinc", zinc_str)

# Validate against the standard ontology
ns = rh.DefNamespace.load_standard()
issues = ns.validate_entity(entity)
```

## Features

- **Kind types** — Marker, NA, Remove, Number (with units), Ref, Uri, Symbol, XStr, Coord, HDateTime, HDict, HGrid, HList
- **Codecs** — Zinc, Trio, JSON, JSON v3, CSV encoding and decoding
- **Filters** — Parse and evaluate Haystack filter expressions against entities
- **EntityGraph** — In-memory graph with CRUD, filter queries, and ref traversal
- **Ontology** — DefNamespace with `is_a`, `fits`, `validate_entity`, subtype/supertype queries
- **Xeto** — Load/unload Xeto libraries, inspect specs and slots, export to Xeto source

## Codecs

Supported MIME types: `"text/zinc"`, `"text/trio"`, `"application/json"`, `"application/json;v=3"`, `"text/csv"`.

```python
# Grid encode/decode
zinc_str = rh.encode_grid("text/zinc", grid)
grid = rh.decode_grid("text/zinc", zinc_str)

# Scalar encode/decode
s = rh.encode_scalar("text/zinc", rh.Number(72.5, "°F"))
v = rh.decode_scalar("text/zinc", s)
```

## EntityGraph

```python
graph = rh.EntityGraph()
graph.add(rh.HDict({"id": rh.Ref("site-1"), "site": rh.Marker(), "dis": "HQ"}))
graph.add(rh.HDict({"id": rh.Ref("ahu-1"), "equip": rh.Marker(), "siteRef": rh.Ref("site-1")}))

results = graph.read("equip")       # filter query
graph.refs_from("ahu-1")            # -> ["site-1"]
graph.refs_to("site-1")             # -> ["ahu-1"]
```

## Ontology & Xeto

```python
ns = rh.DefNamespace.load_standard()

ns.is_a("ahu", "equip")            # True
ns.fits(entity, "site")            # True
ns.subtypes("equip")               # ["ahu", "vav", ...]
ns.validate_entity(entity)         # list of issue strings

# Load custom Xeto libraries
ns.load_xeto(source_text, "myLib")
spec = ns.get_spec("myLib::MyType")
```

## License

MIT
