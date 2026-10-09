# Pinned Xeto readById admission profile

The native bootstrap profile admits readById and its required type/metadata
closure; the HTTP profile adds the selected nine read/system functions and their reachable types/errors from
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

The native manifest selects seven **unchanged** upstream files, including the original
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
python3 haystack-core/xeto-profiles/read-by-id/verify.py --http
python3 haystack-core/xeto-profiles/read-by-id/verify.py --protocol
# Optionally materialize the selected sources for inspection:
python3 haystack-core/xeto-profiles/read-by-id/verify.py --output /tmp/read-by-id-sources
```

The Rust loader embeds these raw files and the manifest, verifies all seven
checksums, and performs the same extraction at load time. It does not need Python,
network access, a checkout of upstream, or generated snippets. The exported
provenance, declaration, augmentation and library views all come from that same
admitted handle. Library views explicitly report `complete: false`.

## Admitted semantics and native API

`haystack_core::xeto::catalog::Catalog::load_pinned()` produces an
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
The legacy loader gains no HTTP, Jeto or full-library semantics from this profile.

## Executable HTTP closure

`load_http_pinned()` uses `http-manifest.json`, verifying all eleven retained
files and their exact line selections. The function augmentation selects
sys.api/funcs.xeto:1-110: readById, readByIds, read, readAll, about, close, ops,
libs and filetypes. The API result closure adds AboutInfo, OpInfo, LibInfo and
FiletypeInfo with explicit scalar and collection fitting. `sys::Filter` and
`sys::Version` retain nominal catalog/revision/qname identity. DateTime, Uri and
None fit their concrete native variants. ApiVersion remains native digit-only
Str, including the previously admitted API error contract.

The complete pinned `sys::TimeZone` declaration has 341 members. Bare enum members
retain metadata; an explicit `key` overrides the programmatic member name, so
`gmtPlus1` has effective key `GMT+1`. Unkeyed members use their exact name. Enum
members are not implicit Marker fields. Admission rejects nullable, typed,
defaulted, nested, query or duplicate effective-key members, unsupported extension,
and tables above 4096 keys, 256 bytes per key or 64 KiB total key text. The default
is not the first member. TimeZone fitting requires exact nominal identity and an
exact member key; `UTC` fits while `utc` and `America/New_York` do not.

The unchanged raw timezones.xeto and Enums.md are retained with hashes and
upstream attribution. Enums.md is checksum-verified evidence, not parsed library
source. The original pin, license and `complete_libraries:false` stay intact;
adding this prerequisite does not admit unrelated enums or complete libraries.

All functions remain `sys::Funcs` augmentation members. `close` requires native
None; null does not fit its native return declaration. Typed result grids validate
every row against their `of` declaration, including required fields, optional
fields and List element types. A generic Dict remains unconstrained for readById
and read. Legitimate structural row-spec information and extension fields remain
preserved. Missing closure members, unresolved `of` targets, duplicate declarations
and unsupported signatures fail admission. The [HTTP executable
profile](../../../docs/typed-http-read.md) documents the application registry and
transport policy separately from core admission.

## Selected protocol-metadata closure (M2-PR07)

`Catalog::load_protocol_pinned()` uses `protocol-manifest.json`: the HTTP closure plus
eight complete selected declarations, each retained with its full body, source file
checksum and line range, and its declaring library's dependency metadata:

| Declaration | Pinned source | Admitted semantics |
|---|---|---|
| `sys::Entity` | sys/types.xeto:178-182 | abstract Dict; required `id: Ref`; nullable `spec: Ref?<of:Spec>` resolved against catalog declarations only |
| `ph::Feature` | ph/kinds.xeto:21-22 | abstract Dict |
| `ph.protocols::ProtocolAddr` | ph.protocols/base.xeto:9-15 | abstract Dict; required `addr: Str` |
| `ph.protocols::ModbusAddr` | ph.protocols/modbus.xeto:17 | inherited `addr` with its literal `pattern`; required `encoding`; `bitIndex: Int?` bounded by `minVal: 0`/`maxVal: 15`; `access` declaration default nominal `r`; nullable `scale`, `byteOrder`, `dis` |
| `ModbusAccess`, `ModbusEncoding`, `ModbusByteOrder` | modbus.xeto:50/57/71 | finite enums with exact keys and nominal identity |
| `ModbusScaleExpr` | modbus.xeto:96 | nominal Scalar identity only; the prose grammar is not enforced or evaluated |

`sys::This` is admitted as the metadata dependency used by `minVal`/`maxVal`; the
unitless source literals bind contextually to the Int constraint without coercing
stored values. `ph` declares `sys.refs`, retained as a dependency-only identity
(583 bytes, SHA-256 `33ef7470…0742`) with no admitted declarations. Every library
reports `complete: false`. `sys::Spec`, `ph::PhEntity` and its global augmentations,
Site/Device, `ph.protocols/globals.xeto`, Query/inverse/transitive traversal and
every other upstream declaration remain unsupported and are not advertised.

`Catalog::with_project(lib, source, markers)` adds one project-owned Dict library
identified by its own digest, never attributed to upstream. Explicit marker
bindings (for example `pump -> fixture::Pump`) are part of the selection.
Strict fitting (`fit_entity`) compiles inherited slots and constraints, checks
nested structural specs, nullable slots, exact enum/nominal provenance and
target-constrained entity references through a caller-controlled record view. It
never inserts declaration defaults or rewrites values; `slot_default` constructs
defaults separately for callers building new data.

Unresolved bases, query and global slots, augmentations, replacement of pinned
libraries and unknown references are resolution errors (issue #21: no selected
type silently matches every entity). The bundled issue #46 `VavZoneAhu` inverse
Query snippet remains an explicit unsupported rejection; no rename is implied.

### Activation

`ActivatedCatalog` binds one selection, its derived compatibility namespace,
declaration provenance and callable codec contexts. A managed graph publishes
exactly one at a time. `SharedGraph::activate_catalog` is reject-on-invalid-
affected-data: the candidate is compiled off-lock, the union of old and candidate
associations (explicit `spec` references into admitted libraries and marker
bindings) is validated over borrowed records, including records an application
hides, in bounded chunks that must all observe the same `GraphState`, and
publication compares the full `GraphState` and observation identity before the
caller's commit point. Entity-only changes restart validation a bounded number
of times, then report `Conflict`. Every attempt scans the whole graph;
incremental revalidation is a follow-up. Unloading cannot discard an existing
obligation. Selection admission (`with_project`) compiles the callable codec
contexts, so selections beyond the codec bounds (256 context definitions,
1024-byte patterns) fail there with the function and declaration named.
Failures publish nothing and emit no wake; success emits one catalog wake after
unlocking. Rejections display without entity ids, values or schema names; the
privileged detail is read explicitly. Runtime catalog generation, the selection
digest and the pinned source revision stored in nominal values remain distinct.

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
