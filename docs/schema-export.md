# Schema export and generated clients

The managed scoped server exposes `GET /api/rusty-haystack/openapi.json` with
`Xeto-Version: 5` and `Accept: application/json`. This is a project-owned extension,
not a catalog Func or the upstream `sys.api::openapi` operation. The legacy CLI demo
profile does not select it. Use the scoped setup in [Shared reads](shared-reads.md)
and provide the ordinary session authentication header when authentication is enabled.

## Document and operation profile

The document is OpenAPI 3.1, using JSON Schema Draft 2020-12. Its paths describe
qualified POST calls such as `/api/sys.api::readById`, with `Xeto-Version: 5`, JSON
argument objects and `Accept: application/json;box=none`. Other runtime methods,
aliases, version 4 and boxing modes remain outside this particular export profile.

Schemas derive from the same immutable admitted catalog and executable registry
as dispatch and `ops`. The fixed handler inventory controls executability;
loading a type or function declaration cannot create a route. Optional history,
watch and command owners and caller policy affect schema visibility exactly as
they affect typed discovery. The endpoint is not itself listed as an executable
Xeto operation.

Every export is caller-scoped, deterministic for equal visible shape and service
selection, and marked `Cache-Control: private, no-store`. It includes no timestamp,
session identifier, global selection digest or hidden source metadata. The original
admission owns work, allocation, output, depth, cancellation and session checks.
Before disclosure, policy scope, owner state and the exact catalog observation
are checked again. An in-flight export becomes unavailable after catalog activation,
even if the visible document would have identical bytes.

Component names escape `_` as `_u` and `:` as `_c`, preserving OpenAPI's name
syntax without collapsing different source identities. `x-xeto-type` retains the
visible source qname. All references are local. The extension
`x-rusty-haystack-json-schema` is a standalone schema library with its own
`$schema`, `$defs` and `#/$defs/…` references. After saving the OpenAPI document:

```sh
python3 -c 'import json; d=json.load(open("openapi.json")); print(json.dumps(d["x-rusty-haystack-json-schema"], indent=2))' > schemas.json
```

Choose a library definition by adding `$ref: "#/$defs/fixture_c_cAsset"` (for
an admitted `fixture::Asset`) to that standalone document. The library alone
does not select one root data type.

## Defaults, nulls and limits

Canonical optional Dict fields may be absent; their presence does not permit
JSON null. Function parameters have a different normalization rule: JSON null
becomes absence before binding. An optional parameter may therefore bind null,
and a parameter with its own default may receive that default. For example,
`readById`'s `checked` defaults to true when omitted or explicitly null. No scalar
construction default is copied into a parameter, and no `default: null` is invented.
Nullable function results preserve literal JSON null.

A generic `Dict` result stays an open generic object. An explicitly typed Asset
can encode its Ref field as a contextual string, but a generic untyped record
may need a boxed Ref to preserve identity. The server's exact `box=none` profile
returns 406 when unboxing would lose information. Schema shape does not bypass
that decision, validate graph references, establish nominal provenance, or prove
an ambiguous union branch. Grid row/column relationships and units also retain
native semantic checks.

A declared Dict shape also accepts an admitted derived Dict in a nested field or
typed List. Its optional `spec` is a structural string, without an enum of
subtypes. JSON validation checks the declared inherited shape; native fitting
checks that the actual structural name exists, derives from that declaration and
satisfies its own stronger constraints. This avoids rejecting valid subtype
values or exposing hidden subtype names. Passing JSON validation alone does not
establish that nominal relationship.

The profile retains inherited slots, constraints and effective requiredness.
Compatible scalar restrictions are flattened; repeated supported patterns are
intersected without weakening either expression. Unsupported regular-expression
syntax, untyped Lists and other unqualified declarations produce visible-only
entries in `x-rusty-haystack-profile.excluded`, with fixed reasons. There is no
silent fallback to an unconstrained type. Excluded dependencies never reveal
hidden names. Definition traversal is bounded to 256 definitions and depth 32,
in addition to the application's original resource limits.

The supported project-pattern subset uses ASCII literals and positive character
classes, ordinary groups and alternation, canonical quantifiers, escaped punctuation and
the common newline/carriage-return/tab/form-feed escapes. Whole-value anchoring
is supplied by the exporter. Unescaped dot, nested or negated classes, Unicode
patterns/shorthands, flags, explicit anchors and other unqualified escapes are
excluded rather than copied into a different regex dialect. These exclusions do
not change native Rust fitting. A quantifier is `?`, `*`, `+`, `{n}`, `{n,}` or
`{n,m}`, with contiguous ASCII digits and at most one lazy `?` suffix. It follows
one atom; another repetition requires an explicit enclosing group. Whitespace
inside counts and stacked repetitions are excluded. Literal braces require
escapes outside a class, and escaped hyphens are supported only inside a class,
preserving the recommended ECMAScript Unicode-mode syntax. Optional, star and
zero-count repetitions retain their empty-string matches.

## Reproduce the external proof

From the repository root, with Rust 1.99.1, uv and Python 3.12:

```sh
uv run --locked --project scripts/schema-client --python 3.12 \
  python scripts/schema-client/validate.py
```

The narrow development environment pins `openapi-python-client==0.29.1`,
`jsonschema==4.26.0` and `openapi-spec-validator==0.9.0` in its own `uv.lock`.
It does not use the product PyO3 environment. The command builds a loopback-only
synthetic fixture with a public test token, admits a project Dict library,
validates the complete export, and generates a Python client without hand edits.
Any generator warning is a failure. Generated code and fixture output are temporary;
the server's owner is closed and awaited on exit. Both generated clients use the
same finite transport timeout. A separate real HTTP proxy stalls an actual
fixture response body to verify that a generated request times out and reaches
owner/child cleanup; an independent watchdog bounds even a broken timeout probe.

The proof checks local reference resolution, deterministic bytes, a standalone
schema library, positive and negative inherited constraints, canonical omission
versus null, and actual nonnull typed records, including a base-typed field and
List containing an explicitly typed derived Dict that passes native fitting.
Generated calls cover omitted
`checked` through `UNSET`, explicit false/null, absent/null IDs, the server default,
400, 403, 404 and 406 typed errors, `ops` Grid output and `about` scalar output. Independent
validators check the actual response bodies. An untyped record's auto-boxed Ref
fails the exported Ref string schema, and the same record remains 406 under
`box=none`.

This qualifies one generator, one locked tool selection, and a local synthetic
HTTP fixture. It does not qualify every OpenAPI generator, the entire Xeto language,
upstream servers, persisted recovery, or physical device effects.
