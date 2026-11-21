# Rich values in read-only MCP tools

Both transports default to `haystack-h4-json-v1`. A trusted launch may instead
select `output_profile = "haystack-jeto-v1"`. Its entity tools remain
`haystack_read` and `haystack_query`, with schemas fixed for its lifetime. Separately
selected history and [H4 semantic resources](mcp-semantic-resources.md) retain
their own authority; resource content stays in the fixed H4 semantic profile. Every
call to the rich profile must acknowledge `"profile":"haystack-jeto-v1"` in its
arguments. Missing or mismatched acknowledgments fail with JSON-RPC `-32602`
before application read admission. H4 calls and schemas retain their original
shape; passing a profile argument to an H4 process is an unknown argument.

```json
{"name":"haystack_read","arguments":{"ids":["site-a"],"profile":"haystack-jeto-v1"}}
```

Clients cannot select boxing, codecs, catalogs, revisions or principal authority
through tool arguments. Launching a rich profile does not grant nominal-value or
function permission. An embedding supplies its own existing read policy. The CLI
retains its explicit entity and hidden-tag scope and adds a bounded nominal
allowlist resolved against the already admitted catalog:

```toml
# Top-level fields for either CLI transport
dataset_file = "entities.json"
dataset_format = "typed-payload-v1"
output_profile = "haystack-jeto-v1"

# Stdio: top level. HTTP: inside each [[subjects]] table.
nominal_specs = ["sys::Version"]
```

All other fields from the [stdio](mcp-stdio.md#standalone-launch) or
[HTTP](mcp-http.md#standalone-launch) configuration remain required. Empty or
omitted `nominal_specs` grants no nominal visibility. At most 256 unique qualified
names of 256 bytes each may identify admitted nominal scalar or enum declarations.
Unknown, native, structured or duplicate declarations fail launch. A nonempty
allowlist with the H4 output profile also fails. The list never imports a library
or admits an additional catalog. Nominal values must match the admitted repository
and revision as well as the allowed qualified spec. Denied nominal provenance,
denied references, and denied identities nested inside a container remove that
entire top-level member through the shared policy.

`typed-payload-v1` is the explicit project-owned input envelope described in
[typed values](typed-values.md#project-owned-payload-v1); it is separate from the
Jeto output profile. Its decoder preserves exact i64 text, floating-point bits,
Buf bytes and nominal provenance. It requires a Grid and retains the existing
4 MiB, 10,000-row, 64-level launch bounds, with a 100,000-value JSON node limit.
The original Trio, Zinc and H4 JSON input formats remain available. Selecting a
typed dataset does not automatically select rich output or nominal visibility.

## Wire representation and provenance

A successful envelope has `profile`, `ok`, `grid`, `complete`, nullable `cursor`,
authorized `row_count`, and `catalog`. The latter contains the admitted upstream
`repository`, `revision`, and local 64-hex-digit `selection` identity. Grid data,
catalog provenance and cursor stamp come from one shared graph read cut. The
selection digest identifies the admitted declaration selection; it is not a new
upstream revision or a claim that the whole upstream catalog is executable.

```json
{
  "profile":"haystack-jeto-v1",
  "ok":true,
  "grid":{
    "spec":"sys::Grid",
    "cols":[{"name":"id"},{"name":"exact"}],
    "rows":[{"id":{"spec":"sys::Ref","val":"site-a"},"exact":{"spec":"sys::Int","val":"9007199254740993"}}]
  },
  "complete":true,
  "cursor":null,
  "row_count":1,
  "catalog":{
    "repository":"https://github.com/Project-Haystack/xeto",
    "revision":"873b922451d3ef4c0c9c08ef3daa542f352d69f3",
    "selection":"<the admitted selection's 64 hexadecimal digits>"
  }
}
```

The profile uses the existing contextual Jeto codec with fixed `Boxing::All` and
requires an exact result. Scalars use `{ "spec": "lib::Type", "val": "text" }`,
with optional Ref `dis`. Structural Grid/Dict/List fields keep their Jeto shape.
The admitted context is borrowed from the same immutable catalog observation used
for the read. Missing context, unknown types, incompatible provenance or invalid
nominal grammar cannot fall back to untyped strings or a standard context.

| Native value | Selected representation and limit |
|---|---|
| Int | Signed i64 decimal text, including both limits and values beyond binary64 integer precision |
| Float / Number | Distinct scalar boxes; signed zero and units retain their native identity |
| Buf | Canonical unpadded base64url text |
| Admitted nominal | Qualified spec plus exact text, interpreted only with the returned admitted provenance |
| Marker / None / NA | `sys::Marker` / `sys::None` / `sys::NA` with `✓` / `∅` / `NA` |
| List Null | JSON `null` element; distinct from None and a missing member |
| Missing dictionary member | Absent; no placeholder or coerced null |
| Native Null dictionary/row/meta member | Whole-page `projection` error because exact member presence cannot survive Jeto absence semantics |
| Remove, Symbol, XStr, Coord; unsupported NaN payload/sign bits | Whole-page `projection` error; no invented Jeto mapping |

The remaining scalar and structural restrictions are those of the
[contextual Jeto codec](jeto.md). A projection can exclude an unsupported member.
Shared Jeto encoding failures expose no partial Grid and publish no pending
continuation. A later MCP wrapping or delivery failure does not roll back a
cursor already published by the shared read; the inherited wrapper and cursor
capacity behavior remains unchanged.
Cursor identity binds the output profile and catalog generation alongside the
existing principal, policy scope, query, projection, page size and graph stamp.

The advertised schema describes this boxed wire representation. Its references
are local, its scalar boxes and envelopes are closed, and null is allowed in
lists but not dictionary members. It does not substitute the separate unboxed
Xeto schema export. Schema validation alone cannot establish signed-i64 range,
canonical base64 trailing bits, nominal grammar, admitted provenance, or complete
Grid column consistency; clients must validate those semantics too.

All preexisting application work/value/depth/retained/output budgets, transport
frame limits, cancellation, absolute deadlines and worker-release ownership apply.
Both rich transports require at least 16 KiB of `max_response_bytes`, enforced
before serving by the library and both CLI launchers. This floor accommodates the
fixed pair of discovery schemas with the full 128-byte request-ID allowance,
including JSON escaping. The H4 minimum remains 8 KiB and both defaults remain
1 MiB; a permitted budget can still reject a tool result that exceeds its bounds.
Encoding consumes the original cumulative budget before cursor publication. Text
content and structured content contain the same envelope, with the original
bounded error codes and `isError` behavior. History, resources, writes, commands,
additional libraries, external hosts and model providers are outside this profile.

## Qualification

The existing installed fixtures in `scripts/mcp/qualify.py` and
`scripts/mcp/qualify_http.py` exercise both default H4 and opt-in Jeto launches.
`scripts/mcp/rich.py` authors typed payload fixtures independently, validates live
input/output schemas with the locked JSON Schema validator, and decodes a bounded
set of admitted values without calling the Rust codec. It checks numeric identity,
nominal provenance, canonical Buf bytes, absence semantics, recursive policy,
acknowledgments, continuations and whole-page failures. Its semantic negatives
include overflow and noncanonical base64. It is not a complete Xeto validator.

Released `rmcp` SDK and raw-wire tests cover both transports. Application tests
cover stale catalog/profile cursors, unavailable/foreign context, encode failure
before cursor publication, budget limits, and interruption while the actual Jeto
encoder owns a worker. Installed fixture evidence remains local-process evidence,
with fixture HTTP keys and numeric loopback; it does not qualify a production
identity provider, TLS deployment, external agent host or model behavior.

The wire rules follow the repository's immutable
[upstream Jeto pin](https://github.com/Project-Haystack/xeto/blob/873b922451d3ef4c0c9c08ef3daa542f352d69f3/src/xeto/doc.xeto/Jeto.md).
