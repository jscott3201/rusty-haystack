# Changelog

## Unreleased

### Security

- Bump `rmcp` from 3.5.1 to 3.8.0 for
  [GHSA-4432-vg92-fqh6](https://github.com/advisories/GHSA-4432-vg92-fqh6) (high): the
  SDK's `StreamableHttpService` cached tool schemas without bound, so a remote client
  could exhaust memory. `haystack-mcp`'s HTTP transport uses that service on MCP
  `2026-07-28`, the affected configuration; exposure was already limited by the
  loopback-only listener and bearer auth ahead of the SDK. Fixed upstream in 3.6.0
  (rust-sdk#1349). The `rmcp` `auth` feature stays disabled, so the OAuth-client
  advisory GHSA-5p52 does not apply. Behaviour changes that come with the bump:
  - A JSON-RPC request ID written in integral float form (`"id":1.0`) is now accepted
    and answered as the integer `1` on both stdio and HTTP, instead of being rejected
    as an invalid ID (rust-sdk#1346). Non-integral (`1.5`) and out-of-range numeric IDs
    are still rejected.
  - On `2026-07-28` the SDK refuses to send server-to-client requests (rust-sdk#1326),
    and its stateless HTTP path answers client Response/Error POSTs with 400. Neither
    changes what `haystack-mcp` does: it never issues server-to-client requests, and
    its HTTP wrapper already rejects client result/error envelopes with
    400/`-32600` before the SDK sees them.

### Added

- `haystack serve --cors-origin <ORIGIN>` (repeatable) and `HaystackServer::with_cors`,
  for dashboards served from an origin other than the server's. Off by default: without
  the flag no CORS headers are sent and browsers apply the same-origin policy unchanged.
  The allowance covers `GET` and `POST` and the `Authorization` and `Content-Type`
  request headers. `Access-Control-Allow-Credentials` is not set, because the server's
  auth is header-based and it sets no cookies. There is no wildcard: `*` is refused with
  a warning rather than forwarded, since `tower-http` panics if one reaches
  `AllowOrigin::list` and an explicit list is the point of the policy.

### Changed

- Salts, nonces and the server secret are generated directly instead of by overwriting a
  zeroed buffer. Behaviour is identical; the old shape tripped a CodeQL
  `hard-coded-cryptographic-value` alert on the salt in `hash_password`, which was a false
  positive — the buffer was overwritten on the next line. (#55)
- Python bindings build on PyO3 0.30.1 and pyo3-async-runtimes 0.30.0 (from 0.29.3 and
  0.29.0), and with Maturin 1.16.0 (from 1.15.0), which `pyproject.toml` now requires as
  the build backend. No binding source changed and the Python API is the same: every
  `#[pyclass]` that Python values are extracted into already opted in with
  `from_py_object`, so PyO3 0.30 no longer deriving `FromPyObject` by default removes
  nothing. The module still declares itself free-threading compatible, the default
  since PyO3 0.28. The supported Python range (3.11+) is unchanged; PyO3 0.30 drops only
  3.8. (#124)
- `roaring` 0.11.5 → 0.12.0 for the tag bitmap index. Behaviour is unchanged; 0.12 adds
  stable-Rust SIMD kernels through two new dependencies, `fearless_simd` (MIT/Apache-2.0)
  and `libm` (MIT). (#124)

### Fixed

- The Zinc parser panicked on input with a non-ASCII char where it expected a fixed-width
  ASCII field — `2024-06-01T00:00:00+0é:00`, for example — because it advanced its byte
  offset by a fixed count and then sliced the string inside a multi-byte char. The server
  decodes untrusted request bodies with this parser and has no catch-panic layer, so the
  panic was reachable from a single request; Trio values, filter literals and the client's
  history decoder reach the same parser. The UTC offset, the time after `T` and time
  seconds now check their bytes are ASCII digits before advancing; `\uXXXX` escapes and the
  date slice with `str::get`, which refuses a range that ends inside a char; `C(lat,lng)`
  scans by char rather than by byte; `ZincParser::new_at` clamps its starting position to
  the input and moves it back to a char boundary; and the parser's position accessors fail
  closed on a misaligned offset. Such input is now a parse error.
- Some malformed Zinc that used to decode is now an error. Time fields were taken as
  fixed-width slices without digit checks, and chrono tolerates leading whitespace, so
  `12:30: 5`, `2024-06-01T 0:00:00Z` and `2024-06-01T00:00:0 Z` — with a space, tab, `\n` or
  `\r` at any time or UTC-offset digit — parsed; reading one value from `12:30:\t2024-06-01`
  decoded `Time(12:30:02)` and left the rest unread. An XStr whose `(` is not followed by a
  quote swallowed one arbitrary character: `Bin( text/plain")` decoded as
  `Bin("ext/plain")` and `Bin(:text/plain")` as `Bin("text/plain")`. In Trio such a value
  now falls back to a plain string, `Str("Bin( text/plain\")")`, as other unparseable Trio
  values do. `"\u+041"` decoded as `"A"` because the hex parse accepted a leading `+`; a
  `\u` escape now requires four hex digits. `C(lat,lng)` trimmed any Unicode whitespace
  around its numbers, such as U+00A0; it now trims ASCII whitespace only.

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
