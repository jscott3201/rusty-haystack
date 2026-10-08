# Contextual Jeto

`haystack_core::codecs::jeto` maps the existing native `Kind` model to the Jeto
contract retained from Project-Haystack/xeto commit
`873b922451d3ef4c0c9c08ef3daa542f352d69f3`. It is separate from Hayson and the
project's typed payload v1. The parser's private JSON syntax tree preserves
numeric lexemes; it is not a second semantic value model. Qualification uses
independent local fixtures and inspected upstream sources, without claiming
execution equivalence against an upstream runtime or complete H5 conformance.

## Closed context and fitting

`Context::standard()` admits the implemented built-in scalar and container
names. `Context::new(catalog, revision, definitions)` adds caller-admitted
nominal patterns, finite enum keys, dictionary member types, list element types, grid row types,
and Ref subtypes. The context is immutable and validates duplicate names,
qualified references, bounded pattern complexity, and compatible grid row
types. Construction validates the codec schema; the caller remains responsible
for the declared catalog provenance. It is not a general library loader,
mutable namespace lookup, or executable registry. Unknown names never fall
back to a legacy fitting path.

Contexts contain at most 256 additional declarations, 256 members per Dict,
256-byte names, and 128 KiB of declaration source. A nominal pattern has at
most 1 KiB source, nesting depth 32, a 64 KiB NFA limit, 256 KiB auxiliary
DFA determinization limit, and 64 KiB completed immutable DFA tables. Context
clones share those tables; membership search has no mutable cache or lazy
allocation. Full-text anchoring is mandatory. Unicode classes, flags,
alternation and repetition are supported when those limits admit them.
Unicode word boundaries (`\b` and `\B`) reject at admission; explicit ASCII
boundaries may be used. State expansion can reject a pattern that would fit a
smaller nondeterministic matcher. These are selected codec complexity
restrictions, not complete Xeto pattern-language admission. Recursive Dict/List
references are permitted: admission resolves names without expanding them,
and decoding/encoding follows only concrete values under the same depth and
node limits. A grid row type must resolve to an admitted Dict.

A `Definition::Enum` contains 1 to 4096 distinct effective keys, each at most
256 bytes and together at most 64 KiB; the overall context bound still applies.
Admission sorts and validates the table once, and context clones share its
immutable storage. Decode and encode charge a conservative binary-search comparison
bound before membership checks. Matching strings become Nominal values with the
exact enum qname, catalog, revision and key; a native Str is not relabeled as an
enum. No first-member default or locale-based timezone normalization is inferred.

An optional qualified expected type applies scalar, member, element or row
context. Explicit boxed scalar types override expected context. An explicit
Dict `spec` overrides the outer Dict context. In grids, column `of` overrides
row-own `spec`, which overrides the grid's default row type. Explicit cell
boxes override all of them. A nested grid owns its own column scope. JSON Bool
retains Bool even in a numeric context. Fitting is a subsequent operation; the
codec does not coerce every value to the expected type or silently guess from
strings/tag names.

## Numeric and scalar identity

A JSON number without a decimal point or exponent defaults to Int; a decimal
point or exponent selects Float. Expected Int, Float or Number overrides that
numeric default. Decimal/exponent Int conversion uses checked decimal digits
before any binary64 conversion, so `9007199254740993.0` under Int retains the
exact integer. Fractional or out-of-range Int values reject. There is no
arbitrary-precision number model. Int scalar text in a box uses integer syntax.

Supported scalars are Bool, Int, Float, Number, Str, Ref, Marker, None, NA, Buf,
Uri, Date, Time, DateTime and admitted nominals. Every scalar box has string
`spec` and string `val`; Ref may additionally have string `dis`. Marker uses
`✓`, None uses `∅`, and NA uses `NA`. Numeric text is chosen to retain binary64
bits and unit identity; a particular finite decimal spelling is not promised.
Canonical positive NaN bits `7ff8000000000000` and infinities roundtrip. Other
NaN payload/sign bits, non-finite Number values with units, and an empty unit
string are explicitly unsupported. Buf uses canonical unpadded base64url,
including canonical trailing bits.

Nominal decoding retains the exact scalar text plus this context's catalog
identity and revision. Encoding requires both to match and validates the
admitted pattern or exact finite enum key. A wire qualified name alone proves no provenance. The
selected exact profile rejects native Str under nominal context because that
combination changes contextual scalar identity in the retained reference
behavior. It does not silently relabel the native Str as a nominal.

Date and DateTime use four-digit calendar years. Time supports up to nine
fractional digits, including qualified minute-boundary leap-second values.
DateTime retains its instant, stored minute-granularity offset, and zone text
without looking up or normalizing an IANA zone. Incoming `Z` without zone text
selects `UTC`; native empty zone text cannot be encoded exactly. Subminute
offsets, non-minute leap fields, out-of-profile calendar text and invalid local
times are unsupported. Remove, Symbol, XStr and Coord are not mapped by this
profile. Boxing cannot make an unsupported scalar grammar exact.

## Containers and wire structure

Lists preserve order and null elements. A decoded Dict null member is absent;
this rule does not alter the native dictionary model. Encoding a native Dict
null member is unsupported because Jeto cannot retain that member's presence.
Native Dict structural `spec` is an undisplayed Ref to an admitted Dict and
maps to a plain wire string. A Dict with a `val` tag remains a Dict when its
`spec` identifies a Dict; it is not mistaken for a scalar box.

A grid contains `spec`, optional `of` and `meta`, ordered `cols`, and ordered
Dict `rows`. Native grid/column Ref-valued `of` metadata moves to the structural
wire position and back. Native custom grid `spec` metadata identifies an
admitted Grid subtype. Generated base `sys::Grid` is elided on decode; a native
redundant base spec tag is rejected because that distinction would be lost.
Structural strings and column names remain unboxed in all mode.

The codec preserves sparse rows without adding cells. Duplicate columns,
unlisted row keys, reserved column name `spec`, wrong metadata types,
displayed structural Refs, unknown grid/column fields, and structural/domain
metadata collisions reject. Grid `of` cannot compete with a subclass's own row
type. These are selected strict profile restrictions. Duplicate JSON keys,
extra scalar-box fields and fractional Int coercion also reject rather than
following permissive reference behavior.

## Boxing and outcomes

`encode`/`encode_metered` return one of:

- `Encoding::Exact(bytes)`: the admitted native information is preserved.
- `Encoding::Lossy { bytes, issues }`: deliberate unboxing has a defined wire
  value but changes identity; issues carry reasons and native value paths.
- `Encoding::Unsupported { issues }`: the profile cannot encode the value;
  there are no output bytes.

`Boxing::Auto` boxes scalar identity that context would otherwise erase;
`Boxing::All` boxes every scalar, while leaving structural fields unboxed.
`Boxing::None` can erase variant, reference display or other type information
and exposes that loss. If unboxed text would not decode under the supplied
context, the result is unsupported. `into_exact()` gives bytes only from an
exact outcome. Syntax, unknown-spec, budget and allocation failures are errors;
no failure exposes a partial encoded document or partially decoded value.
The [HTTP read profile](typed-http-read.md) uses `into_exact()` for every mode.

## Bounded execution

Standalone `decode` and `encode` accept `Limits`. Defaults are 1 MiB input and
output, eight million work units, 32 MiB cumulative retained-byte reservations,
100,000 nodes and depth 64. Depth is capped at 64 in the standalone profile.
Counters include parser syntax, contextual value construction, generated boxes
and structural fields, escaping, tree comparisons, and output growth. Retained
cost is cumulative, including freed temporary storage; it is not live heap
usage or a throughput measurement. Private object and column indexes use
precharged Vec capacity and one in-place sort. Unescaped strings borrow input
only within the temporary syntax tree; owned native conversion is charged
before copying. Escaped strings use one precharged destination. Quoting
reserves its complete escaped span and writes directly to metered output. Native
Dict table payload is reserved before construction. These source/layout
bounds exclude allocator metadata and do not establish measured total heap
allocations.

Native Dict iteration is charged before scanning against a conservative
bucket bound retained through growth and deletion. Insertion capacity alone
can undercount tombstones. `HDict::from_tags` therefore moves entries once into
a fresh table at construction, preserving values while intentionally changing
capacity/allocation behavior; metered construction uses a fallible fresh-table
constructor directly. Native Eq, Hash, Debug and serialized value identity
exclude the internal scan bound. Caller-owned source storage is not charged as
new output memory.

`decode_metered`, `decode_scalar_text_metered` and `encode_metered` accept the
caller's `Meter`. The scalar-text entrypoint applies an explicit expected type
directly, avoiding an intermediate escaped JSON buffer for GET arguments. Work,
retained bytes and nodes are cumulative increments; Input and Output report
document byte lengths; Depth reports the current nesting depth. Implementers
must enforce their limits and check cancellation/deadline at each call. The
original meter error is preserved as `Error::Budget(error)`. An embedding that
already reserved raw transport input validates the Input length without
charging the raw bytes twice. It must not reset counters, deadlines or resource
ownership between codec stages. The application adapter shares its existing
admission, cumulative counters, absolute deadline, cancellation and worker
lease through optional response compression.
