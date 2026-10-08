# Typed HTTP read profile

`GET` or `POST /api/readById` and `/api/sys.api::readById` execute the pinned
`sys.api::readById` signature through the application's existing read service.
The service loads the immutable profile at construction. The native signature
profile remains separate from its HTTP error closure; neither admits complete
`sys` or `sys.api` libraries. The shared [contextual Jeto codec](jeto.md) handles
admitted native values. A unified executable function registry, broader catalog
admission, generated clients, and complete H5 conformance remain outside this
profile.

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

Only `id` and `checked` bind. Every `xeto-*` query name is reserved and excluded
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
parameter's own default just as absent cells do. The typed adapter
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

A null, missing or denied id is indistinguishable: `checked=false` returns null;
`checked=true` produces the same UnknownEntity error for both. Version-4
operation failure remains HTTP 200 with an error grid. Transport, fitting,
media, admission and processing failures use terminal ApiErr JSON regardless
of Accept. Terminal envelopes have fixed fields and no trace or request data;
UnknownFuncErr may include a bounded function name of at most 256 bytes.
This bounded error path remains available after a request budget or deadline
is exhausted. Unsupported methods, including HEAD, return 501; readById has
`noSideEffects` and permits GET. This slice introduces no side-effecting typed
function and therefore no reachable typed GET-side-effect 405 path.

## Authentication and ownership

Authentication precedes version resolution. The existing H4 profile keeps its
401 bearer rejection behavior. Explicit v5 typed requests use 403 for rejected
or absent bearer credentials and 400 for malformed Authorization. SCRAM's
existing `/api/about` challenge/continuation headers, empty handshake bodies,
and successful Authentication-Info response remain on their existing path.
Configured CORS origins stay unchanged; Xeto-Version is added only to the
allowed and exposed header sets. Trusted custom routes and fallbacks retain their authority for both API and
non-API paths. Authenticated custom fallbacks retain bearer enforcement.

One admission owns raw URI/header/body reservation, contextual decode, signature
fitting, the authorized read, result fitting, encoding and optional gzip. Input
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
