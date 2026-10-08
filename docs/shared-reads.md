# Shared application reads

`rusty-haystack-app` provides one `ReadService` for embedding and the HTTP server's
`ScopedReadService` profile. Authorization, filtering, reference traversal,
catalog interpretation, paging, budgets, and output projection run in that
service. `EntityGraph` remains an unrestricted in-process data structure.

## Embedding and HTTP setup

Create an `ApplicationBuilder` with a graph, an explicit `ReadPolicy`, and finite
`ReadLimits`. Its early handle supplies the exact managed service to every adapter.
The policy receives `Principal::Anonymous`, `Authenticated { subject, permissions }`,
or `TrustedEmbedding { subject }`. Trusted embedding is an explicit identity;
the supplied policy still decides what it can read. `AllowAll` is an explicit
unrestricted policy for trusted compatibility applications.

```rust,ignore
let application = ApplicationBuilder::new(graph.clone(), policy, ReadLimits::default())?;
let handle = application.handle();
let reads = handle.read_service();
let listener = HaystackServer::new(graph)
    .with_scoped_reads(handle)
    .with_auth(auth_manager)
    .port(0)
    .into_listener();
let owner = application.owned_resource(listener)
    .start(&tokio::runtime::Handle::current())?;
let address = owner.ready().await?.listeners[0].address;
let page = reads.read(
    ReadContext::with_timeout(
        Principal::TrustedEmbedding { subject: "local-operator".into() },
        Duration::from_secs(5),
    ),
    ReadRequest::new(ReadQuery::Filter("site".into()), OutputProfile::Typed),
).await?;

let closed = owner.close().await;
owner.terminated().await; // also await this after a close timeout
closed?;
```

The HTTP adapter passes the authenticated username and effective coarse
permissions. With authentication disabled it passes `Anonymous`; it does not
substitute an unrestricted identity. The service uses the supplied graph even
if the server builder was constructed with a different one.

`HaystackServer::new` preserves the legacy coarse-permission API for compatibility.
`.with_legacy_unrestricted()` also selects it explicitly. In that profile entity
reads and other handlers retain their legacy contracts. `with_scoped_reads`
selects the restricted capability set:

| Scoped operation | Behavior |
| --- | --- |
| `read` | ID requests or filters over authorized views |
| `nav` | Authorized root sites or records referencing an authorized parent |
| `defs`, `libs`, `specs`, `spec` | Bounded authorized catalog reads |
| `about`, `ops`, `formats`, `close` | Static capability/authentication information and logout |

`export`, `pointWrite`, `invokeAction`, `import`, `loadLib`, `unloadLib`,
`exportLib`, and `validate` have no routes in the scoped profile. Other
capabilities are explicit application selections: `MutationService` adds
`entityBatch`, `entityReceipt` and versioned `changes`; a selected history
provider enables `hisRead`, and `HistoryMutationService` adds `hisWrite` and
`hisReceipt`. `StateSubscriptionService` adds the scoped HTTP/watch operations
and negotiated WebSocket profile without enabling entity writes. See the
[entity mutation](entity-mutations.md), [history read](history-reads.md),
[history mutation](history-mutations.md), and [state subscription](state-subscriptions.md)
contracts. One capability registry drives routing and `ops`. Unavailable routes
return 404 before body decoding or provider callbacks.

Custom routers receive privileged application state. Both `with_router` and
`with_authenticated_router` therefore cause scoped startup to fail before binding
unless `.with_trusted_external_routes()` explicitly accepts them as a separate
trusted authority. Built-in authentication on a custom route does not apply
resource-level read policy inside its handler.

## Policy and snapshot rules

`ReadPolicy::snapshot` returns an immutable `PolicySnapshot` for each request,
including every continuation. Its scope key must change whenever any effective
authorization decision changes. Callbacks are trusted local code and must return
quickly without blocking or acquiring application graph locks.

Policy decides operations, entities, tags, reference targets, reference displays,
catalog entries, and nominal-value provenance. Masking runs before predicates,
projection, and returned-row counts. Thus `not secret` matches an entity whose
`secret` tag is hidden. Forward/inverse references use the same authorized view.
A resource denied by policy and an unknown ID are both omitted from ID reads.

If a permitted tag contains any denied reference, its entire top-level value is
omitted. This includes references nested in lists, dictionaries, grid metadata,
column metadata, and grid rows. Only the returned root entity's own `id` has an
identity exemption; nested `id` values do not. Reference display text has its own
permission, including on the root ID. Hidden catalog terms and unknown terms have
the same external query-validation or unavailable-spec result. Hidden nominal
specifications, libraries, or provenance hide the corresponding value.

Each read holds one bounded graph read guard covering entity revision, catalog
generation, namespace Arc, reference resolution, evaluation, and output creation.
The graph is the sole catalog authority. Legacy library mutation builds from a
captured immutable namespace outside the graph lock, then compares the captured
catalog generation when publishing. A concurrent publication returns a conflict.
Catalog changes do not masquerade as entity/watch changes.

Controlled fitting preserves the pure graph's rule that an absent base stops an
inheritance walk. A base that exists but is denied by catalog policy instead
makes the fit fail. This distinction also applies to a query slot's `of` type;
a hidden base cannot remove inherited requirements from a visible child.

## Pages and output profiles

ID lists are deduplicated and ordered by stable Ref ID. Filter pages scan the
same stable order, independent of recycled internal graph index IDs. `page_size`
is positive and bounded. Results carry `complete`; incomplete pages also carry
an opaque cursor. Denied rows do not contribute to page row counts or completion.
When a budget is exhausted, the result is an error rather than successful truncation.

A cursor is a random handle authenticated with a service-local MAC. Its bounded,
expiring server-side record binds dataset and graph incarnation, entity/catalog
generations, principal, policy scope, normalized query, projection, page size,
and output profile. The token carries no entity ID or principal text. Any bound
change requires restarting the query. Cursor capacity exhaustion is explicit;
valid cursors are not evicted to hide capacity pressure. Reusing a valid cursor
is permitted, but generating additional continuation tokens consumes capacity
until expiry. Service clones share cursor state; a new service does not.

`OutputProfile::Typed` returns a rich `HGrid`. `OutputProfile::H4(codec)` returns
actual encoded bytes for Zinc, H4 JSON, or v3 JSON. HTTP sends those same bytes.
The selected codec's null/missing, metadata, temporal, and numeric rendering
semantics remain observable; this is not a promise of lossless typed round trips.
Rich values without strict H4 representation produce `Projection`. Trio output
is rejected because it drops grid-level completion and cursor metadata. Scoped
`formats` advertises the three output profiles; legacy Trio behavior remains.

HTTP requests use the ordinary request grid plus optional grid metadata:

- `limit`: positive integer page size (legacy first-row `limit` is also accepted).
- `cursor`: continuation string from the prior response.
- `select`: list of tag-name strings. An empty selection means all permitted tags.

Resend the same query and metadata with the cursor; changing them invalidates it.

## Bounds and request lifetime

Defaults include 64 KiB input, 512 IDs, 256 AST nodes and depth 32, 10,000 raw
candidates, 4,096 forward and 4,096 inverse edges, 100,000 value nodes, four million
work units, 16 MiB conservative allocation accounting, value depth 64, 1,000 rows,
1 MiB conservative output bound, a 64 KiB regex source ceiling, and a 256 KiB
compiled-regex/cache bound. The service admits eight requests and queues sixteen, with a five-second maximum absolute
deadline. Cursor defaults are 128 records, 4 MiB, and a 60-second TTL.

These are conservative ceilings, not promised usable capacities. Allocation
accounting includes temporary allocations even after release, decoder expansion,
wide sparse grids, and worst-case escaping before an encoder allocates output.
One large value or a mostly denied inverse traversal can exhaust a budget. Raw
candidate/edge work is charged before authorization, including denied edges.
Fallible evaluation propagates exhaustion/cancellation; it cannot become a false
predicate or successful `not`/optional query result.

Regex source length is checked independently of the NFA/cache limits. In the
locked regex 1.13.1 and regex-syntax 0.8.11, AST parsing and HIR translation happen
before the compiled-size limit is enforced. Before constructing the builder,
the service reserves 16 KiB plus 1 KiB per source byte and 512 KiB per potential
Unicode/class expansion site (every backslash or opening bracket, deliberately
including escaped literals). These allowances cover boxed nodes, parser stacks,
HIR properties and the locked Unicode tables' range/folding scratch. Separately,
regex-automata 0.4.18 compiler scratch is reserved for every pattern: 1 MiB for
its fixed UTF-8 tables/frontier plus sixteen times the compiled-size limit for
cached transition keys and intermediate state/remapping storage. This covers
implicit Unicode classes such as `.` and case-folded literals, too; even an
ASCII pattern requires the conservative unconditional allowance. The locked
meta engine uses forward compilation, reverse compilation with shrinking
disabled, and a possible reverse-prefix attempt. Compiled and search-cache
bounds remain separately reserved. A retained-memory error can therefore occur
below the source-byte ceiling. Raising only the source ceiling cannot bypass the
allocation budget; the reservations require review when regex dependencies or
compiler configuration change.

The HTTP adapter acquires one logical permit before collecting the body. The same
permit moves into decoding/evaluation. The absolute deadline includes body
collection, queueing, graph-lock waits, and execution. A caller that drops its
future signals cancellation. Explicit cancellation and deadline expiry return a
prompt request error; that response is not a receipt that the worker has exited.
A queued or running blocking worker retains its permit until the task actually
releases its capture. Tokio may leave an aborted closure queued behind unrelated
blocking work, so the caller does not await that closure on its stop path.
Cancellation of running code is cooperative between bounded operations, not an
unsafe thread interruption. Ordinary completion is joined. The service borrows
the caller's Tokio runtime. Managed services register before body collection or
queueing and the application retains ownership until actual worker exit. Its
close protocol seals new admission, drains admitted work, then requests stop;
see [application lifecycle](application-lifecycle.md). A retained managed service
returns `Closed` after shutdown. `ReadService::new` remains a separate unmanaged
compatibility constructor whose outstanding workers belong to the caller's
runtime. Dropping a service clone does not stop other users.

Transport-independent errors include invalid query, unavailable/forbidden,
stale cursor, capacity, cancellation/deadline, budget, and strict projection.
HTTP maps them respectively to 400, 404/403, 409, 429, 408, 413 for input or 422
for other budgets, and 422. Error grids contain generic diagnostics and no denied
identifier or display text. Managed `NotReady` and `Closed` errors map to HTTP 503.
Invalid startup limits are configuration errors.
