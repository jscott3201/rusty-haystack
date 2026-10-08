# Pinned Xeto readById admission profile

This package admits one function and its required type/metadata closure from
[Project-Haystack/xeto at 873b922451d3ef4c0c9c08ef3daa542f352d69f3](https://github.com/Project-Haystack/xeto/tree/873b922451d3ef4c0c9c08ef3daa542f352d69f3).
It is a bootstrap subset of `sys` and `sys.api`, **not complete upstream libraries**.
The pinned build properties say version `5.0.0`, maturity `alpha`, and dependency
version `5.0.0`. This profile does not claim that the official public v4 Xeto
documentation describes every construct at this newer source pin.

The exact selected declaration in `src/xeto/sys.api/funcs.xeto` is:

```xeto
+Funcs {
  readById: Func <op, noSideEffects> { id: Ref?, checked: Bool "true", returns: Dict? }
}
```

The real pinned source omits a colon after `+Funcs`; that source grammar is the
admission authority. The function is declared by `sys.api`, with membership in
`sys::Funcs`. The augmentation and its synthesized `mixin` metadata remain
separate from the original `sys::Funcs` type.

## Provenance and reproducible extraction

`upstream/` contains seven **unchanged** upstream files, including the original
Academic Free License 3.0, attribution headers, build variables, both library
pragmas, type definitions, the Spec metadata schema, and API functions.
[manifest.json](manifest.json) records their original repository paths, SHA-256
checksums and selected inclusive one-based line ranges. Concatenate ranges in
order with one extra blank line between ranges. Expand `BuildVar "name"` with
JSON-quoted values from the retained `xeto-build.props`; unknown variables fail.
No selected declaration is renamed or rewritten. Metadata selection retains the
original Spec wrapper only to parse its chosen fields; it does not admit `Spec`.

Verify all retained bytes and reproduce source selections locally:

```sh
python3 haystack-core/xeto-profiles/read-by-id/verify.py
# Optionally materialize the selected sources for inspection:
python3 haystack-core/xeto-profiles/read-by-id/verify.py --output /tmp/read-by-id-sources
```

The Rust loader embeds these raw files and the manifest, verifies all seven
checksums, and performs the same extraction at load time. It does not need Python,
network access, a checkout of upstream, or generated snippets. The exported
provenance, declaration, augmentation and library views all come from that same
admitted handle. Library views explicitly report `complete: false`.

## Admitted semantics and native API

`haystack_core::xeto::read_by_id::ReadByIdProfile::load_pinned()` produces an
immutable catalog. Its 11 types are `Obj`, `Scalar`, `Marker`, `Str`, `Bool`, `Ref`,
`Collection`, `Dict`, `Interface`, `Func`, and `Funcs`; its sole function is
`sys.api::readById`. Metadata fields admitted from the Spec schema are `abstract`,
`doc`, `maybe`, `mixin`, `noSideEffects`, `noInherit`, `op`, `pattern`, `sealed`, and
`val`. Other upstream declarations, functions and metadata are not advertised.
The `noInherit` metadata on `abstract` and `sealed` controls effective metadata.

`fit_arguments("sys.api::readById", &dict)` returns `BoundArguments` containing
native `Kind` values and an `ArgumentOrigin` for each parameter:

- An absent `id` binds `Kind::Null` with `MissingNullable`; an explicit null retains
  `Explicit`. A present Ref fits; a present Str, Bool or typed `Kind::None` fails.
- An absent `checked` binds typed `Kind::Bool(true)` with `ParameterDefault`.
  Explicit `false` is valid. An explicit null, string or marker fails.
- Only parameter-owned defaults bind missing arguments. `sys::Ref`'s construction
  default `@x` is preserved as type metadata but never fills an absent `id`.
- `returns` is excluded from arguments; unknown arguments are rejected.
- `fit_result` accepts a native Dict or null. An unconstrained Dict retains its
  rich values, including Int, Float, None and Buf. Typed None is not a null result.

Quoted scalar defaults remain lexical strings in the parse AST and become typed
values during resolution. Defaults are not invariants. Missing/null/typed-None
remain distinct; the input dictionary is never changed by binding. A later wire
codec must implement its own representation rules, including any null-to-absence
mapping, separately from this native contract.

Source-integrity, parse, resolution, and fitting errors have separate variants.
Parse errors map back to the original selected upstream line. Resolution errors
include source and declaration/member identity. Fitting errors identify the
parameter/result without including request contents. Qualified references must
exist in the admitted catalog and obey declared dependency visibility; nested
slot references are checked recursively before unsupported nested constraints
are rejected. Base cycles and duplicate members fail admission. Parser input is
bounded to 10 MiB and 64 delimiter nesting levels.

The legacy generic parser/loader/fitter is a separate compatibility surface. The
parser now retains augmentation syntax, while the legacy loader rejects it with
an explicit admission-profile diagnostic. It does not acquire full H5 semantics.
This slice adds no HTTP endpoint, Jeto codec, device behavior, or full-library
schema support.

## Independent validation evidence

The integration tests transcribe the selected upstream signature, exact base
chain, default types, nullable positions and allowed catalog membership directly.
They are not generated from the loader's output. An additional independent
behavior oracle is Haxall commit
[`aded27993c2d4834eca4b44ca55671417a1a7ea2`](https://github.com/haxall/haxall/blob/aded27993c2d4834eca4b44ca55671417a1a7ea2/src/test/testHx/fan/Api5Test.fan#L228):
`Api5Test.fan` lines 70–71 and 228–231 establish omitted-ID behavior and explicitly
exclude the inherited Ref `@x` default. That upstream fixture was inspected; it
was not executed here. Its source SHA-256 is
`74229423abf5d02e7c983c4767da465461b3c3cc303b4c1b62c31e501e704345`.
The native Rust tests exercise those expectations independently of HTTP decoding.

## License and updates

The retained upstream originals and excerpts are attributed to Project Haystack
and remain under [AFL-3.0](upstream/LICENSE); the repository's MIT license does not
replace their license. The raw files are unchanged. The manifest, extraction
implementation and admission code are project-owned additions. Updating the pin,
selected closure, build expansion or supported semantics requires an explicit
compatibility update with new checksums and independently reviewed expectations.
