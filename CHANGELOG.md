# Changelog

## Unreleased

### Added

- `haystack serve --cors-origin <ORIGIN>` (repeatable) and `HaystackServer::with_cors`,
  for dashboards served from an origin other than the server's. Off by default: without
  the flag no CORS headers are sent and browsers apply the same-origin policy unchanged.
  The allowance covers `GET` and `POST` and the `Authorization` and `Content-Type`
  request headers. `Access-Control-Allow-Credentials` is not set, because the server's
  auth is header-based and it sets no cookies.

### Changed

- Salts, nonces and the server secret are generated directly instead of by overwriting a
  zeroed buffer. Behaviour is identical; the old shape tripped a CodeQL
  `hard-coded-cryptographic-value` alert on the salt in `hash_password`, which was a false
  positive — the buffer was overwritten on the next line. (#55)

## 0.9.0

Spec-match filters (`ph::Ahu`) did not work. `DefNamespace::fits` returned `true` for any
type name it had never heard of, so every such filter matched every entity — which meant
nothing underneath had ever been exercised. Fixing that predicate exposed four more
defects in the same path. Most of this release is that chain.

### Breaking

- `EntityGraph::from_grid` takes `Option<Arc<DefNamespace>>` instead of
  `Option<DefNamespace>`. `with_namespace` takes `impl Into<Arc<DefNamespace>>`, so
  existing callers passing a bare `DefNamespace` are unaffected.
- `EntityGraph::read`, `read_all`, and `equip_points` now return `Err` for a filter naming
  a spec the namespace cannot resolve, where they previously returned rows. The Python
  `matches_filter` raises `FilterError` in the same case.
- A `ph::X` term resolving to a def now matches entities that **are** an X, not entities
  that carry X's mandatory markers. Row counts change: on the demo dataset `ph::Sensor`
  goes 36 → 12, `ph::Vav` 8 → 6, `ph::Floor` 0 → 3.
- `FitIssue` gains `UnknownType` and is now `#[non_exhaustive]`, so future variants are
  not breaking.
- `xeto::fitting::fits` and `fits_explain` take `&DefNamespace` rather than `&mut`.

### Fixed

- `DefNamespace::fits` returned `true` for any unregistered type name, because
  `mandatory_tags` yields an empty set for an unknown name and `.all()` over an empty
  iterator is vacuously true. A typo in a filter widened a query to the whole graph
  instead of narrowing it. (#12)
- Spec terms were reduced to `lowercase(bare_name)` and looked up only in the taxonomy,
  which misses both type systems a namespace holds: 0 of 23 bundled Xeto specs and 0 of
  241 camelCase defs resolved. `DefNamespace::resolve_spec_term` now tries the exact Xeto
  qname, the bare name as written, then the lowercased form.
- `ph::<Def>` used conformance semantics rather than membership. 579 of the 719 standard
  defs have no mandatory markers, so those names matched everything; `ph::Floor` matched
  nothing, because the mandatory `space` marker is routinely absent from real data. (#20)
- `EntityGraph.with_namespace` and `from_grid` moved the namespace out of the caller's
  Python object, leaving it alive and empty with nothing to signal it. `DefNamespace` is
  now `Clone` and shared as an `Arc`; the bindings fork with `Arc::make_mut`, so one
  namespace can back any number of graphs. `HaystackServer.with_namespace` no longer
  hollows its argument either. (#13)
- Neither `haystack serve` nor `haystack export` attached the ontology to the graph they
  built — only to the server, a different owner — so every spec filter was answered from
  a namespace-less graph. `export --filter 'ph::Ahu'` exited 1 telling a shell user to
  call a Rust constructor.
- JSON v3 located a datetime's timezone name by scanning backwards for an offset. That
  rejected the `Z UTC` form Niagara and SkySpark emit, folded a doubled space into the
  name, and sliced by byte offset — so a non-ASCII byte after a `-` panicked mid-codepoint
  on the `decode_grid` path a server exposes. (#7)
- Zinc truncated `GMT+5` to `GMT` while its encoder wrote `GMT+5` back out, silently
  changing the zone on every round trip. Trio inherited it.
- All four codecs now default an absent timezone name to UTC; v3 and v4 previously
  returned an empty name.
- `DefNamespace::fits` was unreachable from the Python filter API for spec-match terms,
  which always evaluated false. (#4)

### Added

- `DefNamespace::entity_is_a`, `has_type`, `resolve_spec_term`, `fits_spec_term`, and
  `SpecTerm`. Exposed in Python as `entity_is_a`.
- `filter::unresolved_specs`, for callers that can report an error rather than silently
  returning no rows.
- `TaxonomyTree::any_is_subtype`, memoising descendant sets. Spec-term scans over 100k
  entities cost 28.7 ms rather than 59.8 ms.
- `.agents/gate.sh`, running CI's checks locally with the same flags.
- First test coverage for `haystack-cli`.

### Notes

`fits` keeps conformance semantics and is still what `validate_entity` uses. The two
questions — "is this entity an X" and "is this entity well-formed as an X" — are both
worth asking; the bug was answering one with the other.
