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
checked nanosecond field. DateTime applies this to the underlying UTC calendar
and time before restoring its exact fixed offset and timezone name. This retains
constructor-admitted leaps on the last representable date as well as historical
offsets containing seconds, without clipping or normalizing either field.
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

## Python values, conversion and typing

Python exposes the shared value model through the root package and matching
`kinds`, `data` and `codecs` submodules. `Int(value)` retains a signed i64,
`Float(value)` retains binary64 identity, and `Float.from_bits(bits)` accepts any
unsigned 64-bit pattern, including signed zero, infinities and NaN payloads.
`NoneValue()` is typed absence; Python `None` remains Haystack Null. `Buf(bytes)`
owns immutable bytes. `Nominal(type_name, catalog_id, schema_id, value)` retains
the qualified spec, catalog identity, catalog revision (`schema_id`) and exact
text. Nominal construction validates identity syntax, not the existence of a
catalog or conformance to its spec. These wrappers are immutable, hashable and
support equality/inequality; use their numeric properties for ordered comparison.

Plain Python integers and floats retain the H4 Number mapping; bool is admitted
first as Bool. A native integer must fit signed i64 **and** be exactly representable
as binary64. Overflow raises `OverflowError`; rounding would raise `ValueError`.
For example `2**53 + 1` requires `Int`, while `2**53 + 2` is admitted as Number.
The same concrete numeric admission applies to Number, Coord and Float
constructors. Bool is not a numeric constructor argument, and arbitrary
`__float__`/`__index__` objects are rejected without executing their hooks.
`int(Number(...))` truncates finite values toward zero only within
`[-2**63, 2**63)`; the upper bound is exclusive.

Number and Coord equality/hashing use core bit identity, with Number's unit also
part of identity: positive and negative zero differ, and equal NaN bit patterns
have equal hashes. Number ordering uses numeric comparison only when units match;
NaN is unordered and differing units return false. Coord has equality/inequality
only. Read-only properties in shipped stubs match these immutable runtime fields.
Ref display remains cosmetic, and DateTime identity retains its existing instant
plus timezone-label semantics.

`HDict` and `HList` accept supported values; nested containers are explicit
`HDict`, `HList` and `HGrid` objects, rather than implicit arbitrary Python
containers. Value traversal is bounded to 64 levels. Accepted-type conversion
errors propagate unchanged, including `UnicodeEncodeError` for lone surrogates
and mutable wrapper borrow errors. Failed construction returns no partial value;
failed assignments leave the destination unchanged. Graph outputs preflight
nested representability before cloning. Graph removal preflights the returned
entity before mutation, under the same write lock for SharedGraph, so a rejected
conversion does not remove the entity or advance its revision.

Native `datetime.date` output requires years 1 through 9999. Native
`datetime.time` input requires `tzinfo is None` and `fold == 0`; output rejects
leap seconds and sub-microsecond precision with `ValueError`. Date/time inputs
read their native stored fields, including timezone and fold, without invoking
subclass property overrides. Raw Python
`datetime.datetime` input is rejected: use HDateTime. HDateTime retains the exact
core nanoseconds and a fixed offset plus opaque Haystack timezone label. It does
not resolve IANA rules or normalize local-time gaps/folds. Its calendar components
and offset require concrete Python integers (including bool and int subclasses),
without arbitrary `__index__` calls. Its `dt()` projection to native datetime
raises `ValueError` for out-of-range local calendars, leap seconds and any loss
of nanosecond precision. Keeping a value in HDateTime or a typed payload preserves
precision even when native datetime projection is impossible.

`encode_typed_value(value) -> str` and `decode_typed_value(payload)` use the
project-owned typed payload v1 and its byte/node/depth bounds. They preserve rich
variant and bit identity, including nested grid metadata, columns and rows.
Malformed/over-budget payloads raise CodecError; valid Date/Time values outside
Python's representable domain raise ValueError before a container is returned.
H4 codecs remain strict: rich values at any depth raise CodecError instead of
being silently projected. The Python API does not expose an implicit lossy
projection.

The explicit Python package carries one canonical `__init__.pyi`, actual
submodule stubs and `py.typed`. Root convenience exports and submodule-only APIs
match runtime locations. Private value aliases are signature helpers, not runtime
imports. Runtime value range/precision checks remain necessary even for a
statically accepted `int`. The Python floor is 3.11; selected local execution uses
CPython 3.12. From a fresh wheel, outside the checkout, run:

```sh
python scripts/python-values/validate.py \
  --wheel /absolute/path/rusty_haystack-<version>-cp312-cp312-<platform>.whl \
  --python /absolute/path/python3.12 \
  --pyright /absolute/path/pyright \
  --watch-fixture /absolute/path/python_watch_fixture \
  --work /absolute/path/fresh-consumer-directory
```

This verifies the installed module origin, exact stub inventory/digests, the
independent `haystack-core/tests/fixtures/typed-v1-entity.json` fixture, runtime
imports and positive/negative static samples with **Pyright 1.1.413**. The positive
installed sample requires zero type errors and retains ten exact
`reportMissingModuleSource` warnings for runtime-verified native submodule
imports: these modules are registered by the extension and have no Python source
files. Other diagnostics fail validation. The negative sample must reject every
marked misuse. Source-stub analysis is separate and can be warning-free. The
consumer performs no network installation. Build the first-party watch fixture with
`cargo build --locked -p rusty-haystack-server --example python_watch_fixture`.
Validation copies that executable and the watch consumer into external scratch,
checks their hashes and the installed private entry helper, and exercises real
HTTP Create/cleanup plus same-session WS delivery/ACK/recovery. Independent known
values, prepared server identities, full replacements/removals, terminal controls,
request sequences and cleanup counts each have rejecting mutation controls.
Binding-internal Rust tests additionally exercise
unrepresentable core values with an explicitly linked trusted CPython library;
rebuilt-extension pytest remains separate evidence. These selected checks do not
establish every platform, free-threaded Python, H5 or release qualification.

The separate [contextual Jeto codec](jeto.md) encodes these native values under
a closed admitted context with explicit exact, lossy and unsupported outcomes.
Typed payload v1 remains the project-owned lossless identity envelope.
