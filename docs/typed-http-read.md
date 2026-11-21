# Typed HTTP executable profile

The installed `sys.api` functions are `readById`, `readByIds`, `read`, `readAll`,
`about`, `close`, `ops`, `libs`, and `filetypes`. Each is available at its simple
and qualified `/api/` name. Select version 5 to use typed dispatch on names
shared with legacy H4 routes, or use the qualified name explicitly. The tenth
binding, [`ph.api::nav`](#navigation), is served only at `/api/ph.api::nav`:
`/api/nav` remains the scoped H4 navigation route for every version.
The sixteen production handlers (including the selected history, watch, and local command functions below) are a fixed application inventory; routes derive
from it, never from a parsed catalog. Their admitted signatures, codec contexts,
selected library versions and source provenance come from the graph's current
`ActivatedCatalog` observation. Binding rejects duplicate qname/version bindings
and unsupported handler/signature shapes before an observation can serve calls,
and an activation candidate that cannot bind the inventory is not published.
Each typed request captures one observation and uses it for resolution,
discovery, argument decoding/defaults/fitting, metadata and result encoding; it
verifies that observation is still current before any graph evaluation and
otherwise fails with `UnavailableErr`. An owned result already produced may
encode with its retained context. `ReadService::typed_functions()` returns an
owned retained view of the bindings that need only that read service. History, watch, and command availability is selected per invocation from the exact owned services; watches and commands additionally require a scoped session. The bootstrap profile and its
expanded HTTP closure remain explicit subsets of `sys` and `sys.api`; the HTTP
closure also admits `ph.api` as a partial library identity holding `nav`, `watchSub`, `watchUnsub`, `watchPoll`, `hisRead`, `hisWrite`, and `pointWrite`,
with `ph` and `sys.refs` as dependency-only identities. None claims complete
library admission. Because the managed namespace is derived from the admitted
selection, scoped H4 `specs`/`spec` reads list those seven declarations and typed `libs`
lists `ph.api`; H4 `libs` (definition libraries) is unchanged. The shared
[contextual Jeto codec](jeto.md) handles admitted native values.

`ReadService::activate_catalog` (trusted embedding only) replaces the observation
with a selected closure such as the [protocol-metadata
profile](../haystack-core/xeto-profiles/read-by-id/README.md#selected-protocol-metadata-closure-m2-pr07)
under the service's admission, cancellation and session, with its own
`CatalogActivationLimits` (deadline bound, cumulative work, per-record retained
bytes, validation chunk size and revalidation bound) rather than per-request
`ReadLimits`; its value depth and deadline bounds (at most 64 and 60 s) are its
own. Sealing is observed during validation and rechecked at the commit point
after the final lock wait. A commit fence resolves a caller stop racing that
point: once the worker commits, the awaiting caller reports the published
result, never a stop. An error therefore means nothing was published, with two
exceptions: a worker panic after the commit point is reported as
`Control(Unavailable)`, and a caller that drops the activation future loses its
outcome. Either caller reconciles through the graph's catalog generation or
`typed_functions().selection_identity()`. Activating a selection identical to
the current one still publishes a new observation: the catalog generation
advances, one catalog wake is emitted, and in-flight typed requests retaining
the previous observation fail with `UnavailableErr` before graph evaluation.
A candidate that cannot bind a supported handler fails with `Unsupported`
naming the declaration. With the pinned profiles this is a defensive guard: a
narrower pinned profile is already rejected as `Catalog` by provenance, and
project libraries cannot remove declarations. Codec-context failures name the
declaration and slot. Invalid affected data,
including hidden records, rejects the activation without publishing or
disclosing hidden identities. Every attempt validates the whole associated
graph in chunks; sustained entity writes end in `Conflict` after the bounded
revalidations. Incremental revalidation and large-graph performance are not
claimed. Trusted namespace replacement (`with_namespace`, `set_namespace`)
re-derives the namespace view of the same selection rather than pairing a new
namespace with stale strict semantics. Scoped legacy `loadLib`/`unloadLib` routes
remain disabled. Generated clients and complete H5 conformance remain outside
this profile.

Structural `spec` and `of` references in returned records are `lib::Name`
catalog names. They follow the policy's catalog and library visibility, not
entity visibility: an entity allow-list keeps them, and a catalog-hidden name is
removed with its enclosing tag.

Typed Jeto grids reserve the `spec` column for structural row typing, so exact
grid output refuses rows that carry a `spec` tag. Typed `readAll`/`readByIds`
over such records therefore fail with `NotAcceptableErr` (406), while
`readById`, whose result is a Dict, encodes them. H4 grid output is unaffected.

The preview exposes only supported, admitted, authorized functions with the exact
`op` marker. Discovery and invocation use the same entry and visibility predicate:
coarse read permission, catalog visibility and the explicit
`PolicySnapshot::function(&FunctionIdentity)` execution decision must all allow it.
`FunctionIdentity` binds qname, selected library version, catalog/revision and
source path/hash. `AllowAll` explicitly permits the function decision; other
policies must implement it. Every invocation obtains a fresh immutable policy
snapshot for function resolution. A successful discovery does not grant later
execution authority.
Qualified names are unambiguous; simple names resolve only among visible
executable entries, and visible collisions return `AmbiguousFuncErr` without
including denied candidates. Non-op and unsupported declarations are not callable.

This selected preview rule follows the pinned HTTP chapter's op boundary. It
intentionally differs from the broader callable-function wording in `sys::Spec`
and Haxall's resolver, which permit selected non-op functions. The pin defines no
literal `allFuncs` endpoint; this implementation adds none. H4 capabilities and
entity `invokeAction` remain in the legacy adapter.

The declaration pin is Project-Haystack/xeto commit
`873b922451d3ef4c0c9c08ef3daa542f352d69f3`. The raw error declarations and their
SHA-256 hashes are retained in `haystack-core/xeto-profiles/read-by-id/`, with
exact extraction ranges in `http-manifest.json`. The wire behavior follows the
same pin's `doc.xeto/HttpApi.md` and `doc.xeto/Jeto.md`. It has local fixture
qualification, not execution-equivalence qualification against an upstream server.

## Requests and versions

Absent version controls select version 4. `Xeto-Version: 5` or
`xeto-version=5` selects version 5. One query control takes precedence over one
header. Repeated occurrences of either version control are rejected, including
identical repeats; this deterministic restriction is an explicit local choice.
Errors report the current `Xeto-Version: 5`. Successful typed reads report the
selected version. Existing H4 routes retain their compatibility behavior; this
slice does not replace their dispatch or claim a complete version-5 route table.

For `readById`, only `id` and `checked` bind; its existing compatibility behavior
ignores additional well-formed argument fields after complete decoding. Every `xeto-*` query name is reserved and excluded
from arguments. Other argument names and extra grid columns cannot confer
projection, cursor, page, or policy authority. GET values beginning with `[` or
`{` are parsed as JSON; other decoded values are contextual scalar text.
An empty GET `id` is a present empty Ref under the pinned Ref pattern; it does
not default to `x`. The immutable codec argument and result contexts are derived recursively from the
admitted function declarations, including List element and metadata row types;
it does not guess types from tag names. POST named JSON is decoded completely
through this context before only the declared arguments bind. Contextual scalar
text and explicit boxes follow the core codec's precedence; native fitting then
enforces parameter types. Duplicate keys, malformed boxes, and unknown specs in
any decoded member are rejected, including members that cannot bind arguments. Dict null removes that argument, allowing the parameter's
own default. Omitted or null id defaults to Null, which behaves as a missing
entity. Omitted or null checked defaults to true. Native explicit-null fitting
remains unchanged.

POST requires Content-Type, including for an empty body, except that H4 `close`
accepts the legacy empty request without that header. Version 4 interprets
bare `application/json` as Hayson; version 5 interprets it as Jeto.
`application/vnd.haystack+json` (optionally `version=4`) selects Hayson in either
version. `text/zinc` accepts grid arguments. `text/jeto` is accepted in version 5.
Grid input uses only the first row's declared arguments; null cells apply the
parameter's own default just as absent cells do. The exception is an `opGrid`
functions (`ph.api::nav`, `ph.api::hisRead`, `ph.api::hisWrite`), whose grid body binds whole as its single Grid
parameter. `ops`, `about`, `close`, `libs`,
and `filetypes` have zero arguments:
undeclared names, including `returns`, are rejected before null-to-absence
normalization, for named JSON, GET parameters and the supported first-row grid
representations. Reserved `xeto-*` query controls stay outside arguments. The typed adapter
requires a Hayson grid envelope; unrelated legacy H4 decoders retain their
existing behavior. Encoded request bodies are currently rejected with 415.

## Response subset

The version-4 default response is Zinc; the version-5 default is Jeto JSON.
`Accept` selects a supported representation, with quality weights resolved by
range specificity so a wildcard cannot override a specific exclusion;
`xeto-filetype` takes precedence and recognizes `zinc`, `hayson`, `json`, and
(version 5) `jeto`. Every JSON response reports `application/json`.
`Accept: application/json;box=auto`, `box=all`, and `box=none` are supported,
also through the `text/jeto` alias. Auto and all must preserve exact native
identity. None succeeds only when the unboxed result is also exact; identity
loss returns 406. This exact-only rule is a selected HTTP restriction, not a
universal Jeto limitation: the core codec exposes deliberate lossy outcomes.
Unknown modes, duplicate box parameters, unsupported formats, and unsupported
media parameters are rejected with 406. Response gzip
is supported with bounded compression under the same admission and budget.
Successful responses vary on Accept, Xeto-Version and Accept-Encoding, while
preserving configured CORS variation.

The codec preserves null results and list elements, Bool, Str, signed 64-bit
Int, binary64 Float and Number, Ref display, Marker, None, NA, canonical
base64url Buf, Uri, admitted Date/Time/DateTime values, lists, dictionaries, and
nested grids. Auto boxing retains types that an untyped JSON position would
lose. Signed zero, supported fractional seconds, stored DateTime offset and
zone text survive. Canonical positive NaN and infinities are supported;
noncanonical NaN bits, unit-bearing non-finite Numbers, empty Number units,
subminute DateTime offsets, and other documented core exclusions return 406.

Grid and column native Ref-valued `of` metadata maps to unboxed structural wire
fields even in all mode. Grid row and column order, sparse cells, domain
metadata, and nested context scope are preserved. Null dictionary members,
ordinary record data in `spec`, invalid grid shapes, and metadata collisions
are rejected when they cannot preserve identity. Function contexts admit only the nominal types reachable from their signatures:
Filter, Version and the finite TimeZone enum where needed. Unrelated nominal
values return 406. The core codec preserves nominal values only under an
explicitly supplied matching catalog identity/revision.
H4 output keeps its existing strict projection and rejects rich values it
cannot preserve.

`ops` returns an actual native `Grid<of:sys.api::OpInfo>`. Each row has a required
`qname:Str` and human-readable `signature:Str`; `doc:Str?` and
`noSideEffects:Marker?` are optional. Signatures are descriptive, not a client
schema. Empty grids fit, while wrong native kinds, missing required row fields
and incorrectly typed optional fields fail fitting. Structural row specifications
remain compatible with contextual Jeto. The production entries are the sixteen functions listed above, subject to caller policy and owned service selection.

A null, missing or denied id is indistinguishable: `checked=false` returns null;
`checked=true` produces the same UnknownEntity error for both. Version-4
operation failure remains HTTP 200 with an error grid. Transport, fitting,
media, admission and processing failures use terminal ApiErr JSON regardless
of Accept. Terminal envelopes have fixed fields and no trace or request data;
UnknownFuncErr may include a bounded function name of at most 256 bytes;
AmbiguousFuncErr includes only bounded visible candidates.
This bounded error path remains available after a request budget or deadline
is exhausted. Unsupported methods, including HEAD, return 501. GET requires the
exact `noSideEffects` marker on the executable entry; absence of a side-effects
flag or read permission cannot grant GET. `close`, `hisWrite`, `watchSub`, `watchUnsub` and `watchPoll` require POST; the other ten bindings carry the marker. The dispatcher rejects GET with
`MethodNotAllowedErr` before executing a binding without that marker. All typed watch aliases enforce this POST requirement.

## System functions

| Function | Native contract and selected behavior |
| --- | --- |
| `readByIds` | `ids:List<of:Ref>`, `checked:Bool=true`, returns Grid. One row per original input position, including duplicates. |
| `read` | `filter:Filter`, `checked:Bool=true`, returns the first authorized matching Dict or null. Selection follows deterministic entity ID order. |
| `readAll` | `filter:Filter`, `opts:Dict?`, returns a Grid of authorized matches. |
| `about` | Returns a fitted AboutInfo with configured server label, actual UTC wall time and boot time, built product version, enabled protocol strings, and the validated caller when present. |
| `close` | Returns native None, represented as v5 JSON null or an H4 empty Grid, after revoking the exact validated session. |
| `libs` | Returns fitted LibInfo rows for admitted libraries allowed by the current catalog policy, ordered by name. These are partial admitted library views. |
| `filetypes` | Returns fitted FiletypeInfo rows for formats actually accepted by the selected protocol version. JSON is an alias rather than an additional format. |

`readByIds` performs one coherent authorized graph read. Unchecked missing and
denied IDs produce all-null positional rows; a nonempty all-missing result
retains an `id` column so Zinc preserves its cardinality. Checked failure returns
no partial rows and does not reveal whether an unavailable ID exists. ID, row,
candidate and copy budgets count repeated positions. Empty input yields an empty
grid.

Filters use the shared bounded parser, evaluator and authorized View. Contextual
Jeto text and explicit `sys::Filter` boxes are accepted; explicit `sys::Str` boxes
fail native fitting. Text from legacy Grid input is adapted to Filter under either
protocol version. Authorization precedes the limit. Supported `readAll` options
are `limit` and `sort`; validated structural `spec:"sys::Dict"` metadata is retained
separately and other spec values reject. The limit
must be a finite, unitless, nonnegative integer within the request's row ceiling;
Int, integral Float and integral Number values are accepted. Without an explicit
limit, exceeding that ceiling fails atomically. The presence of the `sort` tag,
including `sort:false`, sorts only the selected bounded rows by their visible
display string, then ID for ties. Jeto null fields follow its normal absence
rule. Unsupported options such as `search` and `gridMeta` reject explicitly,
including unsupported keys whose Jeto value is null.

`ApplicationBuilder::server_name` configures a nonempty label of at most 256 bytes
without control characters. The default is `rusty-haystack`. A managed application
captures its boot wall time once at owner start; unmanaged `ReadService::new`
captures it at service construction. Clones share that timestamp. `tz` is the
admitted nominal `sys::TimeZone` key `UTC`; protocol versions are `"4"` and `"5"`.
No vendor identity or library-wide runtime capability is invented.

Typed result fitting precedes H4 metadata projection. H4 about maps only TimeZone
to its exact key string; the timestamps remain DateTime. H4 libs retains the
project's two-column `name`/`version` Str representation. H4 filetypes uses
`def:Symbol(filetype:<name>)`, a `filetype` Marker, and `dis`/`mime`/`fileExt` Str
fields; it omits unowned icon/doc and the H5 capability fields. The selected typed
metadata functions reject legacy filter/limit controls. V5 about and libs keep
nominal identities, so requesting legacy media for them returns 406 under this
preview's exact-output restriction. This is narrower than the general pinned
legacy-media bridge. Unqualified H4 about/read/libs retain their separate existing
compatibility adapters.

## Navigation

`ph.api::nav` is the selected `Func <op, opGrid, noSideEffects> { req: Grid,
returns: Grid }` from the pinned `ph.api` library, admitted as a [partial library
identity](../haystack-core/xeto-profiles/read-by-id/README.md#selected-navigation-function-m2-pr05).
It binds only when its admitted signature carries all three markers and exactly
`req: Grid` and `returns: Grid`. Its typed route is the qualified
`/api/ph.api::nav`; the shared simple name `/api/nav` is an existing H4 capability
and keeps serving scoped H4 navigation unchanged, including for explicit
version 5 requests. Typed `ops` lists the qualified binding, while H4 `ops`
keeps its single `nav` capability row. Discovery and invocation additionally
require the policy's `ReadOperation::Nav` decision, so a principal denied H4
navigation neither sees nor calls the typed binding (`UnknownFuncErr`).

The request follows the pinned HTTP chapter's `opGrid` rule. A Haystack grid
body (Zinc, Hayson, or version 4 `application/json`) is the request grid itself
and binds whole as `req`; its first row is not mapped to named arguments. A
version 5 Jeto body is a named request whose `req` member is a Jeto grid, and a
GET passes that grid as JSON query text. `req` is required: an empty body, `{}`
or a GET without it is `InvalidArgsErr`, as are other names such as `navId`.
The request grid is either empty (any placeholder columns, such as Zinc's
`empty`) or one row whose only column is `navId`. An empty grid, a null or
absent navId and an empty string select the root; a Str or Ref navId selects
that entity. Other navId kinds, additional rows or columns, and the H4 paging
and projection controls `limit`, `cursor` and `select` in grid metadata are
rejected as `InvalidArgsErr` rather than ignored.

Execution uses the request's retained catalog observation and verifies it is
still the graph's current one before any graph read; a request that captured an
observation replaced by an activation fails with `UnavailableErr` and returns no
rows. The root lists the caller's visible sites: records the authorized View
returns with a `site` tag after entity and tag masking. A child level is one
inverse hop: the target must pass the entity and reference decisions, every
inbound edge (including denied and duplicate ones) is charged before any record
copy, each source is masked first, and its relation must survive masking as a
top-level non-id Ref to the target. That is the scoped H4 relation, so nested
references are not navigation edges. Traversal never recurses, so reference
cycles and self references terminate; a source related through several tags
appears once. Rows follow entity id (B-tree) order.

Rows use the H4 `nav_row` shape: `navId` (the entity id as Str), `id` (the
masked Ref, with display only when policy allows) and `dis`. The result grid
always carries a `navId` column; `id` and `dis` columns appear only when rows
carry them, and a row whose `id` tag is masked is a leaf with no navId. Grid
metadata `complete` is a Bool. One request returns at most `ReadLimits::max_rows`
rows: a larger level is truncated deterministically to its first rows in id
order with `complete: false`. There is no typed continuation cursor; scoped H4
`/api/nav` keeps its cursor paging, whose first page under the same bound has the
same rows, and its cursors become stale when the catalog generation changes.
A hidden, missing, reference-denied, unsupported (for example path-like) or
childless target is the same empty complete page, so the response is not an
existence oracle; version 4 receives an ordinary empty grid, not an error grid.
Truncation applies to rows only; the request budgets still bind, and any
budget failure is one atomic `InvalidArgsErr` with no partial rows. The root
scans every entity in id order at one candidate each, from what remains of
`max_candidates` after function resolution spends one per fixed binding (sixteen
today, so 9,984 entities at the default 10,000), but copies a record through
the masked View only when its raw form has `site` and the policy allows the
entity and its `site` tag. Masking
only removes tags, so this skip changes no result; it keeps non-site records
from consuming the copy budgets. A child level charges every inbound edge,
including denied and duplicate edges, against `max_inverse_edges` (default
4,096) before copying any record, so a target with more inbound edges fails
even though at most `max_rows` rows would return. The other budgets are
conservative reservations computed from the charges, not heap measurements,
and were observed on 9-tag point records under default limits:

| Stage | Per row or record | Default budget |
| --- | --- | --- |
| Masked copy of a candidate | about 128 bytes plus 700 bytes retained per visible tag (about 6.4 KiB), 2 value nodes per tag | `max_retained_bytes` 16 MiB, `max_value_nodes` 100,000 |
| Navigation row | about 1.7 KiB retained, about 260 work units including its masked copy | |
| Exact Jeto output of a `navId`/`id`/`dis` row | about 3,800 work units and 5.9 KiB retained | `max_work` 4,000,000 |

At these rates the work budget binds first for version 5 Jeto output: a level
of about 980 or more such rows fails rather than truncating at the default
`max_rows` of 1,000, while version 4 H4 output of the same level truncates at
1,000 rows. Deployments that need full default-size Jeto pages raise `max_work`
or lower `max_rows`. Records with more than about 13 visible tags (about 700
bytes each per masked candidate) exhaust the retained budget before work, so
raising `max_work` alone is not enough for them. Before the root skip, masking
every non-site record exhausted the retained budget near 2,600 such records.

Result fitting precedes encoding. Exact Jeto output boxes the `id` Ref, so
`box=none` returns `NotAcceptableErr` (406) whenever a row carries one. The
version 4 bridge needs no legacy projection: native Grid results now keep their
own metadata through the H4 grid bridge, so nav's `complete` marker reaches
H4 clients, while the bridge grid synthesized for Dict, null and None results
still omits it. The selected `pointWrite` binding is described below.

## Single-point history (M2-PR10)

The pinned declarations are `ph.api::hisRead: Func <op, opGrid, noSideEffects>
{ req: Grid, returns: Grid }` and `ph.api::hisWrite: Func <op, opGrid>
{ req: Grid, returns: Grid }`. Qualified routes use typed dispatch; the simple
`hisRead`/`hisWrite` routes select it with version 5 and retain their existing
H4 behavior otherwise. Grid media binds the entire Grid as `req`. Named Jeto
uses `{ "req": <Grid> }`; GET accepts a structured `req` for reads only.
Discovery and resolution hide functions whose owned service is not selected.
The mutation service must share the exact selected history provider and read
admission. A parsed declaration cannot install a provider or authorize writes.

Read requests contain one `id` Ref and `range` Str row. Unmarked requests return
an ordinary timestamp/value Grid only when collection is `Complete`. A provider
failure or incomplete plain read is an explicit error. The existing
`bounded-history-v1` Str metadata control opts into results carrying actual
coverage, returned samples, range, generation observations and terminal reason;
a bounded partial result never masquerades as a complete page. The original
absolute deadline, cancellation, session fence and cumulative budget cover
transport, binding, provider pulls, fitting and encoding. Typed collectors
retain the binding catalog observation through each stage and check it under
the graph lock used for read observation or write schema preparation. Publication
rechecks the prepared graph state and the exact session fence. A catalog change
after publication is reconciled as uncertainty rather than a no-effect rejection.
Typed collectors protect final encoder headroom before pulls; default limits can produce a
`Limited` result before the configured 1,000-row maximum. This does not assert
an atomic entity/history snapshot or completeness beyond retained history.

Writes require the existing `history-write-v1` submit control in `historyWrite`
Str metadata, an `id` Ref, and ordered `ts`/`val` samples. This is a project
extension within the generic upstream Grid signature. The empty outcome Grid
contains the same explicit committed/rejected/unknown control as scoped H4.
No-control writes and multipoint shapes reject. Original duplicate order,
number bits, unit and timestamp spelling are retained for intent binding;
identical authorized retries resolve the retained outcome and changed intent
conflicts. Last duplicate instant wins within one accepted point batch.

History storage remains Bool/Number/Str plus NA. Null or missing values,
Int/Float, None/Remove and richer values are not coerced. Supported structural
column `of` context is checked before removing it for the existing strict
history decoder: Ref/Str for read id/range, DateTime for ts, and
Number/Bool/Str for val. Arbitrary column metadata and unsupported contexts
reject. Reads convert valid explicit cross-zone range boundaries to the point
zone and use an exclusive end; writes require exact point-zone spelling and
valid historical offsets. Existing H4 admission restrictions still apply.

Before submission the adapter reserves and encodes an identity-only Unknown
fallback under the same budget. A later fitting, encoding or compression failure
cannot imply that publication failed. Revoked/expired sessions still fail final
disclosure, including the Unknown envelope; callers retain their submitted
identity and use the existing reauthorized `hisReceipt` operation to reconcile.
There is no `ph.api::hisReceipt`, automatic replay, multipoint transaction,
equipment command, or disk/restart durability claim. See
[history mutation receipts](history-mutations.md) for the retained authority and
qualification boundaries.

Authenticated typed reads require coarse `read` permission. Typed history
writes require independent coarse `write` permission, even with
`AllowAllHistoryMutations`; write-only users can invoke writes without acquiring
read access. Per-function/catalog visibility and the selected mutation policy
remain additional checks. Final response loss remains uncertainty for the
caller regardless of a locally completed response.

## Authentication and ownership

Authentication precedes version resolution. Version-5 about, read, libs, ops and
close enter typed dispatch before their legacy routes or public/SCRAM bypasses. Only GET `/api/ops` with absent or explicitly
selected v4 controls retains public H4 `name`/`summary` discovery. Unsupported,
duplicate and other version selections authenticate before their error response;
query-over-header precedence includes percent-encoded control names. The existing H4 profile keeps its
401 bearer rejection behavior. Explicit v5 typed requests use 403 for rejected
or absent bearer credentials and 400 for malformed Authorization. SCRAM's
existing `/api/about` challenge/continuation headers, empty handshake bodies,
and successful Authentication-Info response remain on their existing path.
Configured CORS origins stay unchanged; Xeto-Version is added only to the
allowed and exposed header sets. Trusted custom routes and fallbacks retain their authority for both API and
non-API paths. Authenticated custom fallbacks retain bearer enforcement.

The transport captures the exact noncredential `SubscriptionSession` returned
by one authentication lookup before waiting for a read slot. Body collection,
queueing, execution and response/publication handoff observe that handle's
revocation and expiry. A replacement bearer binding cannot retarget a request;
principal/session mismatches reject, and arguments cannot select session authority.
Authenticated H4 built-in reads also retain this authority; custom routers retain
their own behavior.

Typed close requires a validated session and the function's permission. Its zero
arguments, result fitting, bounded encoding and optional gzip complete before
revocation. Only that successful invocation can disclose its prepared acknowledgment
after its own revocation; caller cancellation, application stop and the absolute
deadline still apply. Authenticated unqualified H4 close uses this same captured
session and acknowledgment ordering; its legacy anonymous no-op remains separate.

Close does not stop ApplicationOwner or another login for the same username.
Bounded subscription maintenance eventually removes the closed session's watches,
creation bindings and reservations. The acknowledgment does not synchronously join
all session workers or subscription tasks. Work leases remain with workers until
actual exit, even when their callers stop waiting. Disclosure fencing applies at
the server's response/publication handoff boundary; it cannot recall bytes already
handed to the network. A lost close acknowledgment remains uncertain and is not
automatically replayed. Later bearer rejection establishes unusability, without
uniquely attributing it to close rather than expiry.

One admission owns raw URI/header/body reservation, contextual decode, signature
fitting, registry discovery or the authorized read, result fitting, encoding and
optional gzip. No handler recursively admits another read. Every scanned registry
entry, including a hidden entry, charges work/candidate limits before its policy
decision. Generated metadata copies reserve their admitted source size before
allocation; input body length does not stand in for generated result size. Row,
retained-byte and output limits fail atomically without partial discovery rows. Input
collection is cumulative and uses geometrically bounded buffer growth. Jeto
parsing, contextual reconstruction, generated scalar boxes, escaped strings,
structural wrappers, and output growth charge the original work, retained-byte,
value-node and depth allowances before allocation. These are conservative
budget reservations, not heap allocation or performance measurements. Every
Jeto member and grid row is decoded within that allowance. Zinc separately
reserves expanded work for every row, including repeated column names, even
though only the first row binds arguments. The same cancellation and absolute deadline apply to queued admission and pending
bodies. The authorized View applies the same entity/tag/reference/nominal
visibility as legacy shared reads; denied ids are never probed through a raw
existence check. Dropping or cancelling the waiting future does not release a
running encoder's slot or owner registration. Those remain with the worker
until it actually exits. Encoded response bytes leave that worker only after
successful fitting and bounded encoding.


## Typed compatibility watches (M2-PR11)

The selected `ph.api::watchSub`, `ph.api::watchUnsub` and `ph.api::watchPoll`
bindings use the application's owned `StateSubscriptionService`. Qualified
routes accept versions 4 and 5; simple `/api/watchSub`, `/api/watchUnsub` and
`/api/watchPoll` select these bindings only with version 5. Their existing H4
routes retain their separate compatibility behavior. All three typed functions
require POST because none has `noSideEffects`. Discovery and invocation require
both `Subscriptions` and `Read` policy decisions, function/catalog visibility,
and an actual scoped authenticated or trusted session. Authentication-disabled
anonymous requests do not advertise or execute these typed bindings. A watch
belongs to its exact login session; a new login with the same username cannot
use it. Typed, native explicit-ACK, and legacy watch identities cannot cross
these owner profiles.

The pinned signatures are `watchSub` and `watchUnsub: Func <op, opGrid> { req:
Grid, returns: Grid }`, and `watchPoll: Func <op> { watchId: Str, refresh:
Marker?, curValSub: Marker?, returns: Grid }`. Grid media supplies the whole
subscribe/unsubscribe request. Version-5 Jeto supplies named `req`; poll takes
named arguments with a Str watchId and optional Markers, never Bool flags.
Qualified version-4 watchPoll reads these arguments from **grid metadata** and
requires no rows. Its successful grid retains `watchId` metadata. Zinc parsing
checks every cell and scalar boundary for all three typed watch operations;
malformed poll input cannot consume pending changes or renew the lease.

Subscribe uses `watchDis: Str` metadata to create or `watchId: Str` to add to an
existing watch. Its rows have one `id` Ref column. Optional `lease` metadata is
a Number with a duration unit: the local policy rejects zero, negative,
nonfinite, unitless, or incompatible values. Assignment defaults to 60 seconds,
is capped by the owner's `max_lease` (300 seconds by default), and is bounded
by the remaining session lifetime. Response metadata reports the opaque watchId
and actual assigned lease in seconds. Requested ordering and duplicate positions
are preserved; missing or newly denied records return all-null positions, with
an `id` column even when every row is null. Duplicate IDs consume one membership.
New denied IDs are omitted from membership. Authorized missing IDs remain
inactive members and count against capacity. Later creation and repeated add
can return their current row but do not activate that old membership: remove
and re-add to activate it. Empty add is accepted and renews the lease.

The first ordinary poll returns all active members. Later polls return active
members changed since the last successful poll; refresh returns all active
members. Poll output is in entity-ID order. Add and remove renew the lease but
preserve that global poll fence, including when an existing member is added
again. A new member changed after that fence may therefore also appear in the
next poll. Each member's enrollment revision excludes deletions before enrollment
from terminal-deletion detection. `curValSub` narrows already authorized rows to
exactly `id`, `curVal`, and `curStatus` columns; it cannot restore masked values.
Successful poll, refresh, add, and remove renew the assigned lease at publication.

Unsubscribe requires watchId metadata, removes its listed IDs, and returns an
empty grid. Empty removal renews without closing; a `close` Marker explicitly
closes. Repeated cleanup of a missing, expired, closed, or terminal identity is
an empty idempotent success. A retained identity belonging to another exact
session is rejected before cleanup. Active members losing authorization are
terminal. Deletion (including delete/recreate), changelog gap/overflow, catalog
or policy change, graph incarnation change, session revocation, expiry and
owner shutdown require recreation; refresh cannot revive a terminal identity.
Terminal poll/add typically reports `UnavailableErr`; permission loss reports
`PermissionErr`. There are no invented tombstones or resync markers on this wire.
Deletion terminality is a selected local difference from Haxall's silent omission.

The adapter retains one request admission, catalog observation, binding policy
scope and cumulative budget through authorized capture, result fitting, and full
output encoding. Capture rejects a different policy scope before copying records;
subsequent checks retain that scope through publication and cleanup. Function
permission from one scope cannot combine with record permission from another.
It prepares output before publishing membership, lease or implicit poll-fence
changes. Known representation or output-budget failure therefore publishes no
successful transition. Graph/record conflicts recapture and re-encode under the
same budget. Terminal safety transitions can still invalidate a watch on an
error. A transport loss or cancellation after publication has an unknown outcome:
there is no creation key, automatic retry, explicit ACK, or replay guarantee.
After a lost poll, explicitly refresh a still-active identity and replace client
state. After lost initial creation, recreation may leave the unknown old watch
until expiry. Sharing the native owner does not give these compatibility calls
the native M1 explicit-ACK recovery contract.

Local fixtures cover HTTP dispatch, membership/fence behavior, masking and exact
sessions, terminal recovery, bounded owner storage, and prepublication failures.
The pinned Xeto and Haxall sources are contract evidence; no upstream server was
executed, and full H5 conformance or durable watch recovery is not claimed.


## Local point command simulation (M2-PR12)

The pinned `ph.api::pointWrite` declaration has required `id: Ref` and nullable
`level: Number?`, `val: Obj?`, `who: Str?`, and `duration: Number?`, returning
`Grid`. It has `op`, with neither `opGrid` nor `noSideEffects`: every action,
including inspection, requires POST. Qualified routes support versions 4 and 5;
the simple `/api/pointWrite` alias selects this typed path with version 5.
The legacy unqualified version-4 route keeps its existing behavior.

An application explicitly constructs `SimulatedCommandService` from its exact
`ReadService`, an independent `CommandPolicy`, a mode, and `CommandLimits`, then
selects it with `ApplicationBuilder::simulated_commands`. Duplicate selection
and a foreign read-service owner are rejected. No selection hides the binding
from invocation/discovery. `InspectOnly` selection permits inspection and rejects
mutation with `NotImplementedErr`; writable selection is an explicit opt-in.
The simulator has one owned expiry task, initialized before listeners and joined
at owner shutdown. It never writes graph `curVal` or talks to equipment.

Native callers use `CommandRequest::inspect`, `set`, or `relinquish` and
`SimulatedCommandService::submit` with their `ReadContext` and exact scoped
`SubscriptionSession`. Native outcomes distinguish `Inspection`, `Accepted`,
`Rejected`, and `Unknown`. HTTP likewise requires an authenticated scoped session;
an anonymous transport cannot mint simulator authority. Trusted embeddings may
explicitly construct a trusted scoped session. Transport `read` is required for
inspection and transport `write` for set/relinquish, independently. Command
policy authorizes the action and point; read policy independently controls
function/catalog visibility, point/schema visibility, and result disclosure.
A denied point has the same terminal envelope as a missing point.

Omitted/null level means inspect. Otherwise level must be a finite, unitless,
integral Number in 1–17. Omitted/null value relinquishes that explicit level;
a nonnull value sets it. Writable points must carry `point` and `writable`
markers and kind `Bool`, `Number`, or `Str`. Values must match that kind. In Jeto,
bare numeric `val` remains Int/Float because its parameter type is Obj, and is
rejected for a Number point; supply a boxed `sys::Number`. Bare booleans and
strings already retain their types. Numeric values must be finite. A unitless
Number inherits the point unit, which is stored explicitly; a supplied unit must
have the same known unit identity (aliases are accepted, conversion between
different units is not). Zinc arguments use exactly one complete row; extra rows,
cells, or scalar suffixes are rejected before dispatch.

There are 17 slots. First nonnull value in ascending priority wins; level 17
starts with the validated `writeDef`, or null. All slots null means effective
null at level 17. Accepted writes return an empty Grid. Inspection returns 17
rows and the `level`, `levelDis`, `val`, and `who` columns. Null cells use the
codec's normal missing-cell representation. `who` is caller-supplied attribution,
never authorization; its local default is the trusted principal subject. It
records the last controller independently of the slot value: explicit
relinquishment records the relinquishing caller's attribution, and automatic
expiry retains the previous attribution. Retained attribution counts toward
the store's byte limit even when the slot value is null.
Inspection masks all four fields through current tag policy, keeping the four
column names. No effective-value, expiry, or other metadata bypasses that masking.

Duration is accepted only for a nonnull level-8 set. It must be a finite positive
Number with a known time unit, checked conversion to a nonzero duration, and the
configured upper bound. Nanosecond through year units are recognized; the pinned
reference's month/year conventions are 30 and 365 days, respectively. Unitless
or unrelated units fail. Replacing or renewing a slot resets its deadline;
relinquishment or an untimed replacement removes it. One monotonic clock drives
both acceptance and expiry. `SimulatorClock::manual` is a trusted local fixture
handle with checked advancement; there is no HTTP clock control. Expiry runs at
or after the deadline, and inspection also applies due expiries. Expiry selection
and clearing share a lock, so an old expiry cannot clear a replacement. Session
revocation does not undo an accepted command or stop its expiry.

Local defaults bound selection to 128 points, 32 pending operations, 16 MiB of
retained state, 4096-byte values/IDs, 256-byte attribution, one-hour overrides,
and a 100 ms maintenance interval. Validated configuration ceilings are 4096
points, 1024 pending operations, 512 MiB retained, 65536-byte values, 1024-byte
attribution, 24-hour overrides, and a one-second maintenance interval. These are
simulator restrictions, not upstream conformance claims. Retained schema and
identity are conservative: incompatible kind/unit/writeDef or point/writable
changes, delete/recreate, graph/catalog replacement, or lost changelog continuity
reject subsequent commands until a fresh simulator selection. Intervening schema
changes remain disqualifying even if later restored. Accepted slots are retained;
rejection does not claim they were undone. Pending requests also fence changed
catalog, schema, graph identity and authorization before dispatch. After any
store wait or failed graph acquisition, the worker refreshes both immutable
policy snapshots against the original command and read scopes, including the
typed binding scope. The final store and graph acquisitions never wait, and
policy callbacks run outside both guards. This follows the policy providers'
versioned immutable-snapshot contract; it does not provide an atomic revocation
transaction across independently managed external policy providers.

A request-local atomic fence separates preparation, dispatch, and abandonment.
Before possible dispatch, cancellation, deadline, shutdown, or validation failure
is a rejection and wins against any later dispatch. After dispatch, loss of an
acknowledgment, worker panic, deadline, cancellation, encoding failure, or lost
disclosure authority is `Unknown`; HTTP uses pinned `sys.api::TimeoutErr` status
504 with `dis: "Command outcome unknown"`. That classification survives the
blocking worker, typed invocation, final response check, and outer owner-stop
middleware. The original request permit and owner work registration stay with
queued/running work through actual exit. Owned socket shutdown may close a connection before its final error envelope;
that missing acknowledgment also remains unknown. After headers, failed body
disclosure terminates the stream without protected values; the transport outcome
is unknown.
There is no client operation ID, receipt, automatic retry, reconciliation, or
claim of rollback. Even `Accepted` confirms only in-process simulator state,
never physical effect, durability, upstream runtime behavior, or full H5 support.
