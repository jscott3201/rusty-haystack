# H4 definition and library resources

Both MCP transports can explicitly select `haystack-h4-semantics-v1`. This adds
policy-filtered `resources/list` and exact `resources/read`, using one shared
application observation and budget. The resource text contains a fixed H4 JSON
projection, independently of the entity tools' `haystack-h4-json-v1` or
`haystack-jeto-v1` presentation. No Jeto conversion of H4 Symbols is attempted.
The default tool-only capability inventory remains unchanged.

## Standalone selection

Add the following top-level field to either existing MCP launch configuration:

```toml
semantic_resources = "haystack-h4-semantics-v1"
```

Stdio grants are top level. HTTP grants belong in each `[[subjects]]` table:

```toml
h4_libraries = ["phIoT"]
h4_definitions = ["ahu", "active", "lib:phIoT"]
```

Selection installs the bundled H4-only namespace (`ph`, `phScience`, `phIoT`,
`phIct`) before application startup. It does not invoke the historical
best-effort Xeto overlay. The application's ordinary selected typed catalog may
coexist with that base, including the same-name Xeto dependency-only `ph`
identity. Resource projection uses the H4 library's metadata and exact definition
membership across typed bootstrap and namespace replacement; that dependency
identity does not admit the Xeto `ph` library or specs. No Xeto resources are
exposed. `nominal_specs` grants
do not grant semantic resources. Selecting a library grants its name/version
metadata; it does **not** grant its member definitions. Each definition must be
named explicitly and its declaring library must also be granted. Empty grants
produce an empty authorized inventory. Up to 32 library and 256 definition grants
are accepted; each name is at most 256 bytes. Unknown names, duplicates,
contradictory grants, and grants without resource selection fail before dataset
access or listener startup.

Entity IDs and hidden-tag scope retain the ordinary launch contract. Resource
selection does not authorize entity reads, history, commands, or mutations.
HTTP reauthenticates each request and applies that subject's shared immutable
policy snapshot. CLI scopes last for the process lifetime; restarting changes
the resource authority even if the config and data bytes are identical.

## Discovery, content, and identity

The selected capability is `resources: {}`. Discovery scans the H4 library and
definition maps once under the same graph guard, charging denied candidates too.
It keeps at most nine borrowed candidates for the MCP page size of eight,
ordered libraries first and then definitions, with lexical names within each
kind. `nextCursor` means more authorized descriptors exist; its absence means
complete enumeration. Budget failure is an error, never silent truncation or an
empty success. No full-catalog sorting/materialization occurs. The selected H4
surface consists of nonempty H4 definition libraries and their exact declaring
members; compatibility Xeto-only libraries have no eligible H4 inventory.

Each read returns exactly one text content with MIME `application/json`. The
text is a JSON object with these fields:

| Field | Meaning |
| --- | --- |
| `profile` | Exactly `haystack-h4-semantics-v1` |
| `uri` | The exact canonical URI requested and returned by discovery |
| `kind`, `name`, `library` | Exact definition or library identity |
| `observation` | Service dataset/authority, graph incarnation, and catalog publication counter |
| `provenance` | `source: attached-h4-namespace`, `projection: def-lib-doc-or-library-version-v1` |
| `grid` | One complete shared H4 JSON row |

Definition rows contain only H4 Symbol `def`, H4 Symbol `lib`, and string `doc`.
Library rows contain string `name` and source metadata string `version`. This
reuses the shared application catalog projection. Raw definition tags,
inheritance, `of`, `tagOn`, library members/dependencies, filesystem paths, and
Xeto sources/specs are absent. Documentation is untrusted narrative data and may
mention other concepts; granting a definition is consent to read that narrative,
not a claim of redacting arbitrary prose mentions. The server never follows a
link, opens a file, executes a prompt, or promotes content to instructions.

The closed URI grammar is:

```text
haystack://<dataset>/<incarnation>/<catalog>/haystack-h4-semantics-v1/<lib|def>/<base64url-library>/<base64url-name>/<binding>
```

The dataset is a random service-local authority, not a file path, durable dataset
ID, source hash, or catalog version. Dataset/incarnation use lowercase 32-digit
hex; catalog uses canonical unsigned decimal; names use unpadded canonical
Base64URL over 1–256 ASCII letters/digits, `_`, `-`, or `:`; binding is a 32-byte
HMAC encoded as unpadded Base64URL. The maximum URI is 1024 bytes. Names are exact,
not aliases or substring filters. Different schemes, authorities, spellings,
percent escapes, dot/path traversal, queries, fragments, and extra components
cannot resolve. The parser does not use a filesystem or network resolver.

Resource and page tokens use separate authenticated domains, distinct from
ordinary query cursors. Bindings include the exact principal, effective scope,
profile, resource kind/name/library, dataset/incarnation/catalog, and, for a page,
its page size. Raw principals, scope keys, permissions, or credentials are never
included in tokens or content. The shared snapshot's `semantic_resource` hook may
narrow ordinary catalog permission; it cannot expand it. Reads recheck both.

Publication of a new namespace/catalog, graph replacement/restore, a changed
principal/scope, or a new service invalidates prior names and continuations.
Ordinary **entity edits do not invalidate** these catalog-only observations.
Library version strings are source metadata and are not publication counters.
Stateless continuations last for the current service/observation; no unbounded
resource or page-token table is retained. A captured response describes its
coherent authorized observation; it is not a guarantee that the catalog remains
unchanged after the read. Rediscover after invalidation.

Missing, denied, malformed, foreign, or stale reads return JSON-RPC `-32602` with
`Resource unavailable or stale; rediscover`. The response does not distinguish
private lookup failures or echo a supplied URI. Other budget/lifecycle failures
are JSON-RPC errors, not tool `isError` envelopes. Modern complete results use
`ttlMs: 0` and `cacheScope: private`; the SDK preserves the negotiated legacy
result shape. Client caches cannot revoke already disclosed copies. In
particular, rmcp's default stale-on-error cache must be bypassed or disabled when
testing server revocation.

`resources/templates/list` returns an empty complete list. Templates,
completions, resource subscriptions, notifications, prompts, entity-resource
payloads, Xeto resources, and writes are not selected. Modern stdio clients send
protocol version/capabilities in **every request's** metadata; `server/discover`
does not initialize subsequent requests. Protected HTTP also requires
`Mcp-Name` equal to `resources/read`'s `params.uri`, including the protocol's
Base64 header form when used. Missing/mismatched names return HTTP400/-32020.

## Bounds and ownership

Selecting resources raises the minimum full response budget to **128 KiB** for
either tool profile; the usual 1 MiB default remains. Selecting history retains
its larger 2 MiB default/minimum. Unselected entity-only floors remain 8 KiB H4
and 16 KiB Jeto. Undersized launches fail before data/listener work.

The longest canonical URI for 256-byte library and definition keys and a 20-digit
catalog counter is below 1024 bytes. Eight descriptors plus one continuation use
a conservative bound under 80 KiB, including worst-case JSON escaping, fixed
metadata, and the full 128-byte escaped request ID allowance. Resource reads
reserve sixfold JSON text escaping plus fixed metadata/provenance and outer
framing, and enforce both application and transport caps. The accepted floor
supports useful one-row reads including maximum-length keys; arbitrary large
documentation may exceed it and fails explicitly. There is no promise that every
possible definition fits the minimum budget.

Application candidate/work/value/depth/retained/output limits remain cumulative.
Content and identity are captured under one graph observation. The final bounded
adapter encoder runs in the original joined application worker, after shared
conservative reservations, retaining admission through its actual exit. An
absolute deadline, cancellation, application stop, or abandoned SDK handler does
not release a physically blocked worker early. Transport request and delivery
leases remain held through actual output/suppression. Slow-output and cleanup
semantics remain those of the existing stdio/HTTP profiles.

Native embedders select `StdioConfig { semantic_resources: true, .. }` or
`HttpConfig.semantic_resources = true`, attach the intended H4 namespace, and
provide operation/catalog permissions through the shared policy. A selected
namespace alone grants nothing. The public joined application APIs are
`list_h4_resources_joined` and `read_h4_resource_joined`; their bounded finalizer
variants retain the same worker through trusted adapter serialization.

## Qualification

```sh
cargo +1.99.1 nextest run --locked -p rusty-haystack-app --test semantic_resources
cargo +1.99.1 nextest run --locked -p rusty-haystack-mcp --features stdio
cargo +1.99.1 nextest run --locked -p rusty-haystack-mcp --features http
cargo +1.99.1 nextest run --locked -p rusty-haystack-mcp --features stdio,http
uv run --locked --project scripts/schema-client --python 3.12 python scripts/mcp/qualify.py
uv run --locked --project scripts/schema-client --python 3.12 python scripts/mcp/qualify_http.py
```

The installed fixtures use fresh temporary Cargo installations, real pipes and
loopback sockets, independent JSON Schema/content validators, literal known
bundled definitions, and rejecting identity/content/provenance mutations. They
exercise both entity-tool profiles, modern/legacy stdio, modern HTTP, pagination,
exact useful reads at the selected floor with maximally escaped request IDs,
strict grants, H4 `ph` alongside the typed dependency identity, cross-subject/restart
rejection, URI headers, and cleanup. Native
tests additionally exercise maximum-length keys, live catalog/scope/incarnation
changes, coherent projection, bounded failures, cancellation/deadline/owner stop,
and actual worker/slow-output release. These establish the selected local
projection and transport behavior, not full ontology/Xeto conformance, durable
identities, public deployment, or external agent-host/model behavior.
