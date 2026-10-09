# Typed HTTP executable profile

The installed `sys.api` functions are `readById`, `readByIds`, `read`, `readAll`,
`about`, `close`, `ops`, `libs`, and `filetypes`. Each is available at its simple
and qualified `/api/` name. Select version 5 to use typed dispatch on names
shared with legacy H4 routes, or use the qualified name explicitly.
The nine production handlers are a fixed application inventory; routes derive
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
owned retained view of the current bindings. The bootstrap profile and its
expanded HTTP closure remain explicit subsets of `sys` and `sys.api`; neither
claims complete library admission. The shared [contextual Jeto codec](jeto.md)
handles admitted native values.

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
policies must implement it. A fresh immutable policy snapshot is obtained once
per invocation. A successful discovery does not grant later execution authority.
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
parameter's own default just as absent cells do. `ops`, `about`, `close`, `libs`,
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
remain compatible with contextual Jeto. The production entries are the nine functions listed above, subject to caller policy.

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
flag or read permission cannot grant GET. `close` requires POST; the other eight
bindings carry the marker. The dispatcher rejects GET with
`MethodNotAllowedErr` before executing a binding without that marker. No typed
watchPoll binding is installed.

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
