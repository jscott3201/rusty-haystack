# Typed values and H4 compatibility

`haystack_core::kinds::Kind` is the value carried by dictionaries, grids and the
entity graph. Rust graph reads retain the following distinctions without fitting
values against a catalog or guessing types from strings, units or tag names.

| Value | Contract |
|---|---|
| `Int(i64)` | Exact signed 64-bit integer, from −9223372036854775808 through 9223372036854775807 |
| `Float(Float)` | IEEE-754 binary64; `Float::from_bits` / `bits` retain every bit pattern, including both zeros, infinities and NaN payloads |
| `Number(Number)` | Existing H4 binary64 value plus an optional exact unit string; no arbitrary integer or decimal precision promise |
| `None` | Typed absence, distinct from `Null`, Marker, NA and Remove |
| Missing | Absence of a dictionary key, including a missing grid-row cell; it is not a `Kind` variant |
| `Buf(Vec<u8>)` | Exact bytes |
| `Nominal(NominalScalar)` | Qualified `library::Spec`, catalog identity, catalog revision and exact scalar text |

`NominalScalar::new` requires a nonempty qualified identity and nonempty catalog
identity/revision. Those strings record provenance; they neither load a catalog
nor establish that the value conforms to its spec. In particular, `"000042"`
remains distinct from `"42"`. The existing XStr type is also distinct from a
nominal value.

## Identity and comparisons

`Kind`, `HDict` and `HGrid` implement `Eq` and `Hash`. Different variants remain
different even when they describe the same number. Float, Number and Coord use
float bits for identity: positive and negative zero differ, equal NaN payloads
are reflexively equal, and different NaN payloads differ. Number unit strings are
compared exactly, including absent versus empty units. Dictionary insertion order
does not affect identity or hashing; list, column and row order does.

Existing H4 Ref equality continues to use the identifier and ignore its cosmetic
display string. DateTime equality continues to use the instant plus timezone name,
not the stored fixed offset. The typed payload retains Ref display and DateTime
offset even when those fields do not affect equality. Canonical payload bytes
are consequently not a substitute for every existing type's equality contract.

Numeric filter comparisons are separate from representation identity. A Rust
`FilterNode::Cmp` can compare Int with Int without conversion to binary64. Float
with Float uses IEEE numeric comparisons: signed zeros compare equal, and NaN is
unordered and numerically unequal to itself. Cross-kind equality is false,
inequality is true, and ordered comparisons are false. Existing Number filter
semantics remain unchanged (bit/unit identity for equality, unit-aware numeric
ordering). The H4 filter parser still constructs Number literals: `v == 42`
does not select `Int(42)` or `Float(42.0)`. There is no new typed filter grammar.
Value indexes remain conservative candidate selectors for the existing Number
and Str paths; rich values are not silently inserted into the Number index.

## Explicit H4 projection

`Kind::project_h4` assesses the complete value and returns one of:

- `H4Projection::Exact(value)`: the H4 semantic value model can retain it.
- `H4Projection::Lossy { value, issues }`: projection erases type or value data.
- `H4Projection::Unsupported { issues }`: no projected value is returned.

Every issue includes a structured reason and path into the original value: tag,
list item, grid metadata, column metadata or row. Traversal is bounded to 64 value
levels. `into_value(ProjectionPolicy::Strict)` rejects every loss;
`ProjectionPolicy::AllowLoss` is the caller's explicit acceptance of semantic
losses. Both reject unsupported values. The source is never changed, and rejection
returns no partially projected container.

Int becomes a unitless Number, reporting type erasure even when exactly
representable; unrepresentable integers additionally report precision loss.
Float becomes a unitless Number and reports type erasure. None becomes Null and
reports loss of the absence distinction. Buf and nominal scalars are unsupported;
there is no String or XStr fallback.

**Semantic exactness is not a wire-roundtrip guarantee.** Zinc grid decoding
collapses explicit Null cells and missing cells. Text encodings can lose NaN
payloads; signed-zero handling and other number details depend on the selected
codec. Zinc cannot encode a Grid as a scalar. CSV has no grid decoder and drops
metadata; Trio also represents records rather than preserving all grid structure.
Legacy H4 encoders also retain their existing field/name validation and encoding
limitations. A caller requiring full wire fidelity must qualify its selected
codec and container profile in addition to semantic projection.

The existing `Codec` trait remains H4-only. All built-in encoder entry points,
including streaming hooks, inspect nested values and otherwise omitted metadata
or extra row tags and return `CodecError::Unprojected` for a rich value. They do
not call projection implicitly. Each successful streaming call only qualifies
its supplied inputs; it does not make separate later row calls transactional.
Xeto source exports likewise reject rich metadata/defaults and now return
`Result<String, CodecError>`; this does not add typed Xeto serialization or fitting.

## Project-owned payload v1

`haystack_core::codecs::typed::{encode, decode, decode_with_limits}` implements a
project-owned UTF-8 JSON envelope. This is **not Jeto, Haystack JSON, or an H5 HTTP
profile**, and it is deliberately not registered through `codec_for`.

```json
{"version":1,"value":{"kind":"int","value":"9007199254740993"}}
```

Every value has an explicit `kind`; all fields below are exact case-sensitive
names. Null and typed None are objects, not bare JSON null values.

| `kind` | Additional fields |
|---|---|
| `null`, `none`, `marker`, `na`, `remove` | None |
| `bool` | `value`: JSON boolean |
| `int` | `value`: canonical signed decimal string; no plus, whitespace, leading zeros, negative zero, fractions or exponent |
| `float` | `bits`: exactly 16 lowercase hexadecimal digits |
| `number` | `bits`: same binary64 format; `unit`: string or null |
| `str`, `uri`, `symbol` | `value`: string |
| `ref` | `value`: identifier string; `display`: string or null |
| `date` | `value`: Chrono date string |
| `time` | `seconds`: u32 whole seconds since midnight, less than 86,400; `nanos`: u32 nanoseconds, less than 2,000,000,000 |
| `dateTime` | `seconds`: canonical i64 Unix timestamp string; `nanos`: u32 nanoseconds, less than 2,000,000,000; `offset`: i32 seconds east of UTC; `timezone`: exact source name |
| `coord` | `lat`, `lng`: binary64 bit strings |
| `xstr` | `name`, `value`: strings |
| `buf` | `base64`: standard alphabet, canonical padding and trailing bits |
| `nominal` | `spec`, `catalog`, `revision`, `value`: strings |
| `list` | `items`: ordered array of typed values |
| `dict` | `tags`: object mapping tag names to typed values |
| `grid` | `meta`: tags object; `cols`: ordered array of `{ "name": string, "meta": tags }`; `rows`: ordered array of tags objects |

The encoder emits nullable `unit` and `display` fields; the decoder also accepts
those optional fields when absent. It never interprets a Number as Int. Date/time
validation uses Chrono's representable ranges. Time stores the whole second and
nanosecond fields independently, including leap nanoseconds after a non-minute
second; display text can make these values indistinguishable from an ordinary
following second. Decoding constructs the checked whole second, then applies the
checked nanosecond field. DateTime does this in UTC before restoring its exact
fixed offset and timezone name, including historical offsets containing seconds.
The Time payload does not accept a text `value` alias. Grid rows retain all
supplied tags, including tags outside the column list. Duplicate column names
are rejected by both encoder and decoder.

Encoding orders dictionary keys lexically and preserves all sequence order.
`decode_with_limits` checks bytes before parsing and uses a bounded JSON visitor
which rejects duplicate object fields before insertion. Escaped equivalent keys
are duplicates. Unknown versions, variants and fields, invalid/range-overflowing
numeric forms, noncanonical binary data and trailing JSON are errors. No graph or
other persistent state is touched during decoding; a complete owned `Kind` is
returned only after validation.

Default bounds are 1 MiB of input, 100,000 JSON values including containers, and
64 JSON nesting levels (the envelope is depth zero). Bounds are configurable;
byte/node limits must be positive and depth must be 1 through 64. JSON structure
adds levels beyond semantic value nesting. The encoder applies the default
bounds to its output; decoding with larger byte/node limits does not imply that
default encoding accepts that document. String sizes and allocation are also
bounded by the admitted byte count. These are payload bounds, not general limits
on in-memory graph size.

## Python compatibility and validation

Plain Python inputs keep the H4 mapping: integers/floats become Number and Python
`None` becomes Null. No Python typed-value API is introduced. Rich Rust values
raise `TypeError` at scalar, list, dictionary, grid, column, graph-read and graph
diff conversion boundaries. A Python graph removal first validates the returned
entity, under the same write lock for SharedGraph, so a rejected conversion does
not remove the entity or advance its revision. Number and Coord Python equality
and hashing delegate to the same core representation identity.

`haystack-core/tests/typed_values.rs` contains independent payload vectors,
integer/binary64 edges, graph reads, nested rejection and scan-versus-index checks.
The fixture `tests/fixtures/typed-v1-entity.json` was authored independently of the
encoder. Binding-internal Rust tests exercise rich values using an explicitly
linked trusted CPython library; normal extension rebuild/pytest validation is a
separate check. Existing H4 codec and Python suites remain the compatibility
corpus. Neither these local checks nor this payload establishes H5, database,
Arrow, external-client or release-artifact qualification.
