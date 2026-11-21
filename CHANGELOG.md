# Changelog

## 0.10.0

0.10.0 adds an application layer, `rusty-haystack-app`, that owns authorization, resource
budgets and lifecycle for embedded Rust, a scoped HTTP profile, Python and a new MCP
adapter, `rusty-haystack-mcp`. Built on it are optional PostgreSQL and SQLite durability,
a pinned, partial Haystack 5 typed HTTP profile with contextual Jeto, Python async and
synchronous clients, and Parquet history capture with optional snapshot SQL
(`rusty-haystack-analytics`). `HaystackServer::new` and `haystack serve` without
`--postgres-config` keep the legacy HTTP profile. SCRAM moves to the published
three-request exchange, which does not interoperate with 0.9.0 and earlier, and the MSRV
rises to 1.99. Most new profiles are explicit subsets; `docs/support-matrix.md` states
what each one is and is not qualified for.

### Breaking

- SCRAM authentication follows the published Haystack exchange: `HELLO`, client-first and
  client-final as three GET requests, unpadded base64url outer fields and response-scoped
  handshake tokens. It replaces the earlier two-request form, so SCRAM between 0.10.0 and
  0.9.0-or-earlier clients or servers fails; stored credentials stay valid. Signatures
  change for `haystack_core::auth::{server_first_message, client_final_message,
  format_www_authenticate}`, `AuthManager::{handle_hello, handle_scram}` and the Python
  `client_final_message` and `format_www_authenticate`. (#68) See `docs/server-api.md`.
- MSRV rises from 1.97 to 1.99 (0.8.1 on crates.io declared 1.94), and the workspace uses
  resolver 3. The repository pins Rust 1.99.1. (#65, #127)
- `Kind` gains `None`, `Int`, `Float`, `Buf` and `Nominal` for exact typed values, so
  exhaustive matches need new arms. H4 codecs and Xeto source export refuse these values
  unless they are explicitly projected, and `xeto::export::{export_spec, export_lib}` now
  return `Result<String, CodecError>`. (#73) See `docs/typed-values.md`.
- `GraphError` gains `ImmutableId` and `RevisionExhausted`. An update that removes,
  retypes or renames an entity's `id` now fails with `ImmutableId` before changing the
  graph; previously the stored `id` changed while the graph kept the old identity. (#66)
- History providers moved from `haystack_server::{his_provider, his_store}` to
  `haystack_app`. `HistoryProvider` reads through bounded sessions (`open`) that end
  Complete, Limited, Interrupted or Failed, replacing `his_read`, and
  `HaystackServer::with_history_provider` takes `Box<dyn haystack_app::HistoryProvider>`.
  (#77) See `docs/history-reads.md`.
- `haystack_server::ws::WatchManager` is removed; watches use the application's shared
  subscription owner. `AppState` changes shape: `graph` is a `GraphView`, `namespace` is a
  method, `his` is optional, application services are new fields, and `ws::ws_handler`
  takes different extractors. (#74, #79) See `docs/state-subscriptions.md`.
- The Rust HTTP client no longer follows redirects, and reqwest retries are off, so an
  operation whose outcome is uncertain is never replayed. (#67) See `docs/client.md`.

### Security

- Bump `rmcp` from 3.8.0 to 3.9.0 for
  [RUSTSEC-2026-0462](https://rustsec.org/advisories/RUSTSEC-2026-0462.html)
  (GHSA-c9f9-cp8h-jcj5): the SDK's built-in Streamable HTTP clients buffered response
  bodies without limit, so a malicious MCP server could exhaust the client's memory.
  `haystack-mcp` ships only the server side; the affected reqwest client is enabled
  only in its dev-dependencies, for its own tests against its own server, and no
  published binary or library build compiles it. The bump clears the advisory for
  `cargo deny`. Nothing observable through `haystack-mcp` changes:
  - 3.9.0 adds the Skills extension (`skills/list`, `skills/get`,
    `resources/directory/read`). `haystack-mcp` does not declare it, and both
    transports still answer those methods as unavailable (`-32601`; 404 on HTTP)
    from their own method allowlists, before the SDK's handler.
  - The SDK's Streamable HTTP server now parses every `Accept` field with quality
    values and matches media types case-insensitively. The HTTP wrapper keeps its
    own stricter `Accept`/`Content-Type` checks and still hands the SDK canonical
    values, so its 406/415 decisions are unchanged.
  - The remaining server-side fixes concern stateful sessions, server-to-client
    requests, `x-mcp-header` parameter headers, the SDK's own stdio transport and
    generated output schemas; `haystack-mcp` uses none of them (stateless HTTP, no
    outbound requests, its own bounded stdio transport and fixed schemas).
- `chrono` and `chrono-tz` were declared unmaintained
  ([RUSTSEC-2026-0543](https://rustsec.org/advisories/RUSTSEC-2026-0543.html),
  [RUSTSEC-2026-0555](https://rustsec.org/advisories/RUSTSEC-2026-0555.html)); no
  patched version exists and neither notice reports a vulnerability. Both are ignored
  in `deny.toml` while the migration is tracked in
  [#134](https://github.com/jscott3201/rusty-haystack/issues/134). Date and time kinds
  carry chrono types in the public API, so that migration will be a breaking change.
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
- An unregistered username with an empty password could complete SCRAM against the
  server's decoy credentials and receive a bearer token, with no permissions. Issuing a
  token now requires a registered user. Affects 0.9.0 and earlier, including 0.8.1. (#68)
- The `rustls` floor rises to 0.23.46 for RUSTSEC-2026-0346, -0347, -0348 and -0349
  (#112), and the lockfile moves past RUSTSEC-2026-0381, -0382, -0383, -0385 and -0386
  in `webpki-roots`, `rustls-webpki`, `webpki-root-certs`, `ctutils` and `tokio` (#122).

### Added

- `rusty-haystack-app`, a new crate. `ApplicationBuilder` produces one `ApplicationOwner`
  and borrowed `ApplicationHandle`s that share startup, admission and close across
  embedded Rust, HTTP listeners and WebSocket tasks. `ReadService` serves authorized,
  bounded reads with policy masking, reference traversal and cursors bound to principal
  and snapshot. Servers opt in with `HaystackServer::with_scoped_reads` or
  `with_application`. Features `postgres` and `sqlite`; none by default. (#74, #75) See
  `docs/shared-reads.md` and `docs/application-lifecycle.md`.
- Selectable application services, in process memory unless a durable backend is chosen:
  atomic entity batches with operation receipts and a revision change feed
  (`entityBatch`, `entityReceipt`, `changes`; #76); bounded history reads (#77) and
  authorized one-point history writes with receipts (#78); session-bound state
  subscriptions shared by native, HTTP and WebSocket callers (#79); and a local
  point-write simulator with no equipment authority (#94). See `docs/entity-mutations.md`,
  `docs/history-reads.md`, `docs/history-mutations.md` and `docs/state-subscriptions.md`.
- PostgreSQL durability (app and CLI feature `postgres`). Entity batches, history,
  receipts and the change feed survive restart over verified TLS, and several application
  processes can share one dataset (#98, #100). `haystack serve --postgres-config FILE
  --users FILE` runs it standalone (#101), and `haystack storage` exports, verifies,
  imports, activates, cancels and reconciles a backup restore (#102). Qualified against
  PostgreSQL 18.6 only; other versions, replication and power loss are not. See
  `docs/postgres-durability.md`, `docs/standalone-postgres.md` and
  `docs/postgres-recovery.md`.
- SQLite durability (app feature `sqlite`): `sqlite::application` keeps entities, the
  change feed and bounded history in one local file with a single owner. There is no CLI
  selector and Python wheels do not include it; power loss, network filesystems and
  multiple owners are not qualified. (#119, #120) See `docs/sqlite-durability.md`.
- A typed HTTP profile, a partial Haystack 5 preview, on servers with an attached
  application. It serves pinned `sys.api` functions (`readById`, `readByIds`, `read`,
  `readAll`, `about`, `close`, `ops`, `libs`, `filetypes`) and `ph.api` functions (`nav`,
  `hisRead`, `hisWrite`, `watchSub`, `watchPoll`, `watchUnsub`, `pointWrite`), selected
  by `Xeto-Version: 5` or the qualified name. Signatures are pinned to Xeto `873b9224`;
  library admission is partial and full Haystack 5 is not claimed. (#80, #81, #83–#85,
  #90, #91, #93, #94) See `docs/typed-http-read.md`.
- Contextual Jeto in core, preserving integer/float distinction, Number bits and units,
  Ref display, buffers and nominal provenance; the typed HTTP profile requires exact
  output or answers 406. A project-owned typed payload carries the same values, and
  explicit H4 projection reports exact, lossy or unsupported. (#73, #82) See
  `docs/jeto.md` and `docs/typed-values.md`.
- `GET /api/rusty-haystack/openapi.json` exports OpenAPI 3.1 and JSON Schema 2020-12 for
  the operations the caller may use on a scoped server. (#95) See `docs/schema-export.md`.
- Xeto repositories: the client lists versions and fetches an exact `.xetolib` with
  SHA-256 provenance (`repoPing`, `repoSearch`, `repoVersions`, `repoFetch`; #96), and an
  embedding can serve a read-only `/repo/` and admit downloaded packages before activating
  a new catalog revision (#97). See `docs/repository-client.md` and
  `docs/repository-serving-admission.md`.
- `rusty-haystack-mcp`, a new crate: read-only MCP tools `haystack_query`,
  `haystack_read` and optional `haystack_history`, plus authorized H4 definition and
  library resources. Features `stdio` and `http`, none by default. The CLI builds them
  with `--features mcp` (`haystack mcp stdio --config FILE`) and `--features mcp-http`
  (`haystack mcp http --config FILE`). HTTP is loopback-only with offline RS256 token
  verification; production identity providers and TLS deployment are not qualified.
  Output is `haystack-h4-json-v1` by default, `haystack-jeto-v1` by opt-in. (#103,
  #107, #109–#112) See `docs/mcp-stdio.md` and `docs/mcp-http.md`.
- `HaystackClient<HttpTransport>::his_read_stream` consumes a finite `bounded-history-v1`
  Zinc response in bounded batches as bytes arrive and validates completion at EOF.
  (#121) See `docs/history-reads.md`.
- Python: `Int`, `Float`, `NoneValue`, `Buf` and `Nominal` value types with typed payload
  codecs, and complete installed stubs with `py.typed` (#104); `AsyncHaystackClient` (#105)
  with bounded async watches (#113) and opt-in Haystack 5 `read_by_id`/`read_by_ids`
  through `H5Config` (#116); `SyncHaystackClient` (#117); embedded in-memory applications
  through `ApplicationBuilder`, `ApplicationOwner` and `ApplicationHandle` (#106); and
  `tls_verify=False` for lab HTTPS with self-signed certificates (#115). See
  `rusty-haystack/README.md`.
- CPython 3.14. CI now tests the bindings on CPython 3.11–3.14 (Linux x86_64), for each
  interpreter both in place and as a wheel installed into a clean environment, and release
  wheels add `cp314` for Linux and macOS on x86_64 and aarch64: 16 wheels plus the
  sdist. Each `cp314` release wheel runs the full Python suite on a runner of its own
  architecture, except macOS x86_64, which is cross-built and not runtime-tested.
  Free-threaded `cp314t` wheels are not published. `requires-python` stays `>=3.11`.
  (#133)
- `haystack serve --cors-origin <ORIGIN>` (repeatable) and `HaystackServer::with_cors`,
  for dashboards served from an origin other than the server's. Off by default: without
  the flag no CORS headers are sent and browsers apply the same-origin policy unchanged.
  The allowance covers `GET` and `POST` and the `Authorization` and `Content-Type`
  request headers. `Access-Control-Allow-Credentials` is not set, because the server's
  auth is header-based and it sets no cookies. There is no wildcard: `*` is refused with
  a warning rather than forwarded, since `tower-http` panics if one reaches
  `AllowOrigin::list` and an explicit list is the point of the policy.
- `rusty-haystack-analytics`, a new library crate that exports one completed remote
  `bounded-history-v1` read (`HaystackClient::his_read_stream`) into bounded Arrow batches
  and an immutable local Parquet dataset, and reopens it with validated bounded readback.
  Values keep their exact kind, binary64 bits, unit spelling, NA and timestamp offset; the
  remote header and its observations are retained in a versioned manifest. Nothing is
  published unless the source ends `Complete`. Publication is a no-replace hard link of a
  synced manifest; a failure after that link is `PublishedUncertain`, distinct from a
  precommit failure, because the dataset is already committed. `Dataset::open` verifies
  hashes and physical layout, including page level and payload framing, before upstream
  decodes anything; reading re-hashes each part's in-memory bytes before decoding them,
  and `Dataset::manifest_sha256` identifies the dataset. Only a fully consumed reader
  reports semantic completeness. Captures are owner-only (`0o700`/`0o600`) on Unix. There
  is no atomic source snapshot, power-loss qualification, overwrite/append/resume, object
  store, CLI, Python or MCP surface (SQL is the optional `sql` feature below), and Windows
  durability is not qualified.
  `arrow-array`, `arrow-schema` and `parquet` are new dependencies, pinned exactly. See
  `docs/parquet-history.md`.
- Snapshot SQL: the optional `sql` feature of `rusty-haystack-analytics` adds
  `haystack_analytics::sql`, bounded read-only DataFusion SQL over Parquet history
  captures. `SqlEngine::builder().register(name, dir)` drains and validates a capture to
  semantic completion, then seals it in memory; queries never touch the filesystem and
  `QueryOutput.sources` reports each scanned snapshot's export id and manifest SHA-256.
  A fixed schema maps NA to NULL with `present = false`, keeps NaN/±Infinity bits, and
  carries `ts` (UTC), `ts_local` (stored offset applied) and the exact timestamp
  components; a `snapshots` table carries provenance. Queries run on an engine-owned
  runtime (32 MiB thread stacks) with process-wide admission, a per-query memory pool
  with spill disabled, a timeout and output caps. Before planning, the SQL text is
  bounded in bytes and in tokens (by an iterative tokenizer, so a long left-deep chain
  such as `1+1+…` is `Error::Limit` rather than a stack overflow), and the parsed
  statement in relations, joins, set operations and CTEs, with CTE references expanded;
  only queries are accepted, and `CUBE`, `ROLLUP`, `GROUPING SETS`, `TABLESAMPLE` and
  locking clauses are refused. During execution, window input is reserved from the pool,
  and per-batch bytes, bytes over the whole plan and join output rows are capped; every
  scan yields before each batch, so a join that loops without output still stops at its
  deadline. Scalar,
  aggregate and window functions and binary operators are allowlisted: string building
  (`||`, `concat`, `replace`, `regexp_replace`, `overlay`, `lpad`/`rpad`/`repeat` and
  similar) and `arrow_cast` are not available in SQL, so concatenate after the query.
  Nothing regex-backed is available either: `regexp_like`, `regexp_count`,
  `regexp_instr`, `regexp_match`, `~`, `~*`, `!~`, `!~*`, `SIMILAR TO`, `LIKE`, `ILIKE`
  and `~~` are rejected, because compiling and matching a pattern allocates outside the
  pool; use `starts_with`, `ends_with`, `contains` and `strpos`. `approx_percentile_cont`
  (whose literal centroid count aborted the process), its `_with_weight` form,
  `array_agg`, `string_agg`, the `nth_value` aggregate and `grouping` are not
  available; every window expression must have a fixed-width type, and the columns a
  window adds are reserved from the pool. `substr`/`substring` are not available (use
  `left`, `right` or `substr_index`). Joins inside parentheses count toward the join
  bound. DDL, DML,
  statements, table functions, URL tables, cross and non-equi joins and recursive queries
  are refused. `Error` gains `Sql` and `Query`, and the `Cancelled`, `Deadline`, `Limit`,
  `Capacity`, `Runtime` and `Worker` messages no longer say "capture". The timeout is best-effort:
  planning and work within one poll cannot be cancelled, and a stuck query keeps its
  process-wide permit. Expression temporaries are not reserved from the pool. See
  `docs/sql-snapshots.md`.
- `haystack_core::codecs::history::result_header_grid` encodes a validated
  `HistoryReadHeader` as the result envelope without collecting its sample rows;
  `result_grid` now builds on it.

### Changed

- The Arrow/Parquet pins move from 59.3.0 to 59.3.2 to share one family with DataFusion
  55.2.0, and `deny.toml` ignores three `zstd` advisories (RUSTSEC-2026-0391, -0398,
  -0403) that arrive only through `arrow-ipc` under the `sql` feature and are unreachable
  from it; tracked in #131 for removal with Arrow 60.
- The optional SQLite profile is re-baselined from SQLite 3.53.2 to **3.54.1**:
  `rusqlite` `=0.40.2` → `=0.41.1` (bundled `libsqlite3-sys` 0.38.2 → 0.39.1). 3.54.1 is
  the 3.54.0 feature release plus a 32-bit heap-buffer-overflow fix and other upstream
  fixes; nothing in either release changes WAL, `synchronous`, journal, checkpoint, busy
  or progress-handler behaviour. Durability verification now rejects engines older than
  3.54.1, and native qualification pins the 3.54.1 version and source ID. (#125)
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
- crates.io publication now includes `rusty-haystack-mcp` and `rusty-haystack-analytics`,
  in the order core, client, app, mcp, analytics, server, CLI. Without mcp on the
  registry the CLI could not publish, because cargo requires optional dependencies to
  exist there too. Every `cargo publish` runs with `--locked`. (#61)
- The GitHub release now waits for crates.io publication, so a failed crate upload no
  longer produces a GitHub release; crates uploaded before the failure stay published.
  PyPI publication is a separate workflow and does not wait for it. (#61)
- Release artifacts are built once and their exact files promoted: each carries a
  source, lockfile and toolchain receipt, and staged bytes are rechecked before
  publication. The Linux x86_64 CLI, a CPython 3.12 wheel and the sdist are installed and
  run before staging; other targets get metadata checks only. Wheels and the sdist now
  carry the `.pyi` stubs and `py.typed`, and the sdist a consistent pruned lockfile. A
  release ships five CLI archives, 16 wheels, the sdist and seven crates. (#72, #118) See
  `docs/artifact-validation.md`.
- JSON v4 decoding rejects a Number whose `unit` is present but not a string, instead of
  dropping it. (#77)
- Project-authored Rust is compiled under `unsafe_code = "forbid"`. (#63)

### Fixed

- Graph queries could omit valid matches through candidate indexes, indexes registered
  after entities were added were not backfilled, stale Ref adjacency was retained, and
  `SharedGraph::write` did not notify subscribers; a changed write batch now notifies once.
  (#66)
- The Rust client lost a configured private CA or client identity after authentication, so
  later requests on fresh connections failed certificate validation. (#67)
- The Rust WebSocket client could not complete the server's watch protocol, and the JSON
  v3 decoder could panic on a non-ASCII string by slicing inside a UTF-8 character. (#69)
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

0.9.0 was merged to `main` but never tagged or published; 0.10.0 is the first published
release that contains it.

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
