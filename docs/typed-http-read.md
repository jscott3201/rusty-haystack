# Typed HTTP executable profile

`GET` or `POST /api/readById` and `/api/sys.api::readById` execute the pinned
`sys.api::readById` signature. Version-5 `/api/ops` and `/api/sys.api::ops`
execute `sys.api::ops`, returning the caller-visible executable operations.
One immutable application registry owns both production bindings, their admitted
signature/profile, codec contexts, selected library version, source provenance,
GET eligibility, and handler selection. Registration rejects duplicate
qname/version bindings and unsupported handler/signature shapes before the
registry becomes available. The native bootstrap profile and its expanded HTTP
closure remain explicit subsets of `sys` and `sys.api`; neither claims complete
library admission. The shared [contextual Jeto codec](jeto.md) handles admitted
native values. Broader catalog admission, generated clients and complete H5
conformance remain outside this profile.

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
not default to `x`. The immutable codec argument context is derived from the
admitted function's actual Ref and Bool parameter declarations at construction;
it does not guess types from tag names. POST named JSON is decoded completely
through this context before only the declared arguments bind. Contextual scalar
text and explicit boxes follow the core codec's precedence; native fitting then
enforces parameter types. Duplicate keys, malformed boxes, and unknown specs in
any decoded member are rejected, including members that cannot bind arguments. Dict null removes that argument, allowing the parameter's
own default. Omitted or null id defaults to Null, which behaves as a missing
entity. Omitted or null checked defaults to true. Native explicit-null fitting
remains unchanged.

POST requires Content-Type, including for an empty body. Version 4 interprets
bare `application/json` as Hayson; version 5 interprets it as Jeto.
`application/vnd.haystack+json` (optionally `version=4`) selects Hayson in either
version. `text/zinc` accepts grid arguments. `text/jeto` is accepted in version 5.
Grid input uses only the first row's declared arguments; null cells apply the
parameter's own default just as absent cells do. `ops` has zero arguments:
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
are rejected when they cannot preserve identity. This service admits no domain
nominal catalog; such results return 406. The core codec can preserve nominal
values only under an explicitly supplied matching catalog identity/revision.
H4 output keeps its existing strict projection and rejects rich values it
cannot preserve.

`ops` returns an actual native `Grid<of:sys.api::OpInfo>`. Each row has a required
`qname:Str` and human-readable `signature:Str`; `doc:Str?` and
`noSideEffects:Marker?` are optional. Signatures are descriptive, not a client
schema. Empty grids fit, while wrong native kinds, missing required row fields
and incorrectly typed optional fields fail fitting. Structural row specifications
remain compatible with contextual Jeto. The production entries are
`sys.api::ops` and `sys.api::readById`, subject to caller policy.

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
flag or read permission cannot grant GET. Both production bindings permit GET;
the dispatcher rejects GET with `MethodNotAllowedErr` before execution for a
binding without that marker. No watchPoll or session-close binding is added.

## Authentication and ownership

Authentication precedes version resolution. Version-5 `ops` enters this path
before the legacy public bypass. Only GET `/api/ops` with absent or explicitly
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

The transport retains the exact noncredential `SubscriptionSession` returned by
its authentication lookup in private admitted invocation state. A replacement
bearer binding cannot retarget an admitted request. The retained session is
checked for revocation at invocation worker entry; principal/session mismatches
are rejected. Session handles cannot be supplied by arguments. Session close
and ongoing body, queue, and disclosure fencing are reserved for PR09.

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
