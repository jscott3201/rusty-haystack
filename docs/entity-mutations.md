# Atomic entity mutations and retained feed

`rusty-haystack-app::MutationService` is an explicit opt-in over the same managed
read service, graph and lifecycle owner. It accepts ordered Add, Patch and Remove
operations, checks current write policy and resource limits, and publishes one
prepared core batch and its receipt under the graph write lock. It does not write
history, issue equipment commands, or enable WebSocket mutation.

## Select the service

Create the mutation service before building an external HTTP router or starting a
listener. `ApplicationBuilder::entity_mutations` rejects a service belonging to a
different managed read authority. Ordinary read authorization grants no write
access. `AllowAllMutations` is an explicit trusted-embedding option; applications
normally implement `MutationPolicy` themselves.

```rust,ignore
let builder = ApplicationBuilder::new(graph.clone(), read_policy, ReadLimits::default())?;
let store = EphemeralMutationStore::new(graph.clone());
let mutations = MutationService::new(
    builder.handle().read_service(), store, mutation_policy, MutationLimits::default(),
)?;
let builder = builder.entity_mutations(mutations.clone())?;
let listener = HaystackServer::new(graph)
    .with_scoped_reads(builder.handle())
    .with_auth(auth_manager)
    .into_listener();
let owner = builder.owned_resource(listener).start(&tokio::runtime::Handle::current())?;
owner.ready().await?;
```

`MutationPolicy` authorizes the intent and every prepared before/after row. Its
reconciliation decision receives the stored original intent, including after an
entity has been removed. Rules must be immutable, quick and nonblocking.
`replace_policy` waits for current authorization/publication decisions and
atomically replaces the rules. A retained provider plan rechecks its authorized
generation at publication and rejects after replacement. Provider callbacks run
outside graph locks. Body admission, absolute deadline, caller cancellation and
actual blocking-worker completion belong to the existing application lifecycle;
a returned timeout is not proof that a worker has stopped. A deferred plan keeps
its admission slot and registered work until publication, rejection or drop. It
rejects publication after application admission seals. Close continues waiting
for ownership to be released and reports an incomplete close if a provider never
releases its plan. New work is rejected after closing begins.

## Identity and outcomes

Every request supplies an operation ID, dataset, graph incarnation and expected
revision. Empty batches and repeated targets fail. An empty patch to an existing
entity creates no revision; a nonempty equal patch and a reference-display-only
patch each create a revision. Revision overflow is rejected before effects.

A binding consists of the stable principal kind/subject, dataset, incarnation and
operation ID. Mutable permission lists and policy generations are excluded.
Current authorization is checked separately on submission and reconciliation.
Canonical typed-v1 bytes bind the expected revision and ordered operations,
including numeric variants, floating-point bits, units, reference displays and
DateTime representation. Dictionary order is normalized; operation order is not.
An authorized identical committed retry returns the original receipt before
checking the now-stale expected revision. A different payload conflicts.

| Outcome | Meaning |
| --- | --- |
| `Committed` | Contains before/after revisions, optional public commit span, and an explicit qualification |
| `Rejected` | This attempt is guaranteed to have caused no effect |
| `Unknown` | Effect is unresolved; preserve identity and explicitly reconcile |

Receipt storage has count and byte limits. New identities are rejected before
mutation when capacity is exhausted. Committed and pending bindings are not
evicted to make an ID reusable. Failures before provider admission create no
binding; a provider-admitted guaranteed rejection remains bound to its original
intent. Dropping an admitted provider plan leaves a pending binding. Missing
receipts are `Unknown`, including after recreation of the store, and never prove
no effect. No mutation is automatically replayed after uncertainty.

The selected store is **ephemeral process memory**. Retaining its handle permits a
new service to reconcile the same surviving state. A new store gets a new dataset;
a newly wrapped or replaced graph gets a new incarnation. `MutationProvider`
fixtures qualify before-commit rejection, abandoned pending work, lost postcommit
acknowledgement and ordering behavior only. They do not qualify disk/database
atomic commit, crashes, uncertain database COMMIT, or multi-process ordering.
Those remain separate storage and distributed-ordering work.

## Complete commit units

Core `GraphDiff` is the sole entity revision authority for native CRUD and service
batches. Its public `CommitSpan { first, last }` contains no principal or operation
ID. A native change is a singleton span; an atomic service batch is one span with
multiple flat diffs. A raw `SharedGraph::write` closure remains nontransactional:
an accepted native prefix survives a returned error or panic, without a service
receipt. Queries and indexes change atomically when a prepared service batch is
published. Notifications are sent after graph unlock and are hints only.

Retention counts actual diffs and conservative owned-value bytes. A complete unit
is appended before complete old units are discarded. The floor is the last wholly
discarded revision. An oversized service unit fails before mutation. An oversized
trusted native diff may still mutate the graph, but creates an explicit gap and
advances the floor before cloning a retained diff. Preparation also bounds existing
index buckets, source traversals, and container growth; a small request can be
rejected when its affected existing data exceeds the work or allocation budget.

A `ChangesRequest` with no cursor bootstraps at the current head. The returned page
identifies dataset/incarnation, head, floor and current position. Subsequent pages
advance only after a complete unit has been evaluated, including units whose rows
are denied. The page limit counts source diffs. A next unit that does not fit the
remaining page is left for the next page; one that cannot fit an empty page returns
`UnitTooLarge` with the caller's cursor unchanged. Units are never split or skipped.
A deadline or incomplete evaluation cannot advance a cursor.

Feed cursors bind principal, current read-policy scope, dataset, incarnation,
catalog generation and typed-v1 profile. They permit later entity revisions. Whole
graph replacement invalidates cursors even at equal/lower revisions, and restoring
a retired graph never revives its former incarnation. Entity, reset and catalog
notifications are distinct; catalog changes do not fabricate entity revisions.
Replay works without receiving any notification. New, changed and removed/preimage
values use the same current entity, tag, nested-reference, reference-display and
nominal-provenance masking as reads. A hidden `id` tag suppresses the feed row.

## HTTP and first-party Rust client

The scoped profile advertises `entityBatch`, `entityReceipt` and versioned `changes`
only when mutations were selected when routing was built. `entityBatch` requires
the coarse `write` permission as well as mutation policy. `entityReceipt` and
`changes` require coarse `read` plus their application decisions. Legacy `import`
and `export` remain unavailable in this profile. The legacy unrestricted `changes`
shape is not accepted by scoped `changes`.

Each extension request and response is a one-row, one-column H4 grid containing a
`payload` STR. Its contents are the strict project typed-v1 envelope and
`entity-v1` schema, defined once in `haystack_core::codecs::entity`. Unknown fields,
versions and noncanonical values are rejected. All public revisions and positions
use decimal strings, including values above 2^53 and up to u64::MAX. Typed encoding
and decoding use the same default 1 MiB document limit. Additional outer grammar,
escaping, nodes, depth, source, page and retained-memory limits apply; configured
application limits may reject an otherwise syntactically valid document. No batch
is silently split. Post-admission outcomes use ordinary success grids without an
`err` marker. Authentication, decode and body-limit failures can be HTTP errors.

`HaystackClient::submit_entities`, `reconcile_entity` and `entity_changes` use the
same core DTOs, without depending on the application crate. Submission makes one
transport call. Transport, response-body and malformed-acknowledgement failures
preserve the identity in `Unknown`; the caller explicitly reconciles. First-party
HTTP construction disables retries and redirects. A transport created with a
caller-supplied reqwest client cannot prove those policies and rejects entity
submission before dispatch; use `connect_with_config`, `HttpTransport::new`,
`with_format`, or `with_bearer_config`. Custom transports explicitly implement the
single-submit `EntityTransport` contract. WebSocket does not implement it.
