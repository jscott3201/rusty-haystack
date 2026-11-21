# Bounded MCP history

`haystack_history` is available only when the managed application explicitly
selects a `HistoryService`. It uses the shared History policy, provider and
`bounded-history-v1` result Grid. Both [stdio](mcp-stdio.md) and protected
[loopback HTTP](mcp-http.md) support it under H4 or launch-selected Jeto.
Entity-only applications continue to advertise `haystack_query` and
`haystack_read`. This selection adds no writes, resources, analytics, snapshot
or continuation authority.

## One finite call

```json
{
  "name": "haystack_history",
  "arguments": {
    "id": "temperature-a",
    "start": "2024-11-03T05:30:00Z UTC",
    "end": "2024-11-03T06:30:01Z UTC",
    "max_rows": 1000,
    "max_bytes": 2097152,
    "timeout_ms": 2000
  }
}
```

Jeto launches additionally require `"profile": "haystack-jeto-v1"`. Calls cannot
select another output codec, provider, catalog, principal or file. IDs omit `@`.
Both required boundary strings must decode as explicit Zinc DateTimes with
consistent named-zone offsets. The range is `[start,end)`; equal boundaries
produce an authorized empty range, and reversed ranges fail. Relative days,
calendar-date shorthand, open-ended ranges and cursors are outside this tool.
The shared parser converts boundary instants to the point's timezone.

Positive row, byte and time caps tighten launch/service limits. The row cap is
applied before provider copies. `max_bytes` bounds a **data-bearing response**,
including JSON-RPC framing, escaping, structured content and its text copy.
Use at least **2 MiB (2,097,152 bytes)** for a usable history budget. A smaller
positive cap can fail before provider open with a `budget` error. Fixed diagnostic
responses are bounded by the launch transport limit; an impossible one-byte
request does not require a one-byte diagnostic.

History selection raises the launch default and minimum response budget to
2 MiB for either output profile. The maximum remains 16 MiB. Existing
entity-only H4 and Jeto launch floors stay 8 KiB and 16 KiB, respectively, with
their 1 MiB default. The history floor accommodates all three discovery schemas,
a maximally escaped 128-byte request ID and useful small history results.
Conservative framing/text reservations grant the shared collector at most
`(max_bytes - 2048) / 8` encoded-grid bytes; its source allowance additionally
reserves 128 KiB for metadata/encoding. A request may reach a source/work limit
before its physical response cap.

`timeout_ms` starts at the original transport request acceptance, is capped by
the launch deadline, and is never renewed after queueing, between pulls, during
join or for encoding. The application's existing maximum duration may tighten
it further. The same tightened deadline also stops blocked physical response
delivery. The provider and its cleanup can outlive a deadline if they ignore
cooperative cancellation; the call continues to hold capacity until their actual
completion. A cancellation/deadline response is never a provider cleanup receipt.

## Values, completeness and errors

A data result contains `profile`, `history_profile: "bounded-history-v1"`,
`ok`, `complete`, `grid`, `row_count`, `response_bytes` and `timeout_ms`. Jeto
also carries the admitted catalog repository/revision/selection. Text content
is exactly the JSON serialization of `structuredContent`.

The Grid's `ts`/`val` rows are strictly ordered by instant. Its `id`, `hisStart`
and `hisEnd` metadata retain the requested point and point-zone boundaries.
The `history` metadata string contains the shared strict typed control envelope:
requested/evaluated range, kind/unit/timezone, returned count, retained endpoints
and count, eviction boundary, selected batch/total/work/response limits, graph
incarnation/revision/catalog generation, opaque policy observation and separate
history authority/incarnation/generation. Counters remain canonical decimal
strings. These observations do not establish an atomic cross-store snapshot.

History values remain **Bool, Number, Str and NA**, even under Jeto. Number bits
and units, nanosecond timestamp precision and historical timezone offsets are
preserved. NA remains distinct from null/None and from an absent timestamp.
Gaps stay absent; no interpolation or fabricated quality columns are added.
`nativeValues=false`, `mode=generationCheckedLive`, `atomicMetadata=false` and
`completeBasis=retainedRecords` describe this finite live-read contract.

`Complete` produces `ok=true`, `complete=true` and a normal MCP tool result.
It covers retained records within the requested interval and does not recover
evicted data. Deliverable `Limited`, `Interrupted` and `Failed` outcomes retain
the authorized partial Grid and explicit terminal/reason, with `ok=false`,
`complete=false` and `isError=true`. A retry creates a new observation; there is
no continuation token. Current policy/catalog checks can withhold a previously
collected prefix if disclosure is no longer authorized or representable.
Transport cancellation, disconnect or deadline may suppress delivery entirely.

Malformed arguments produce invalid-params errors. Pre-collection failures use
the existing sanitized `{profile, ok:false, error:{code}}` envelope. Missing and
denied points both yield `unavailable` without provider access. A principal that
lacks History operation permission receives `forbidden`. Hidden required
schema/sample tags also prevent provider access. Instruction-like sample text
remains untrusted data.

## Explicit standalone selection

Add these top-level fields to a stdio configuration:

```toml
history_file = "history.json"
history_read = true
max_response_bytes = 2097152
```

For HTTP, `history_file` and the response cap are top-level; `history_read = true`
belongs inside each authorized `[[subjects]]` table. Both default History
permissions are false. Selecting a provider alone does not grant the operation,
and `history_read = true` without a history file fails launch validation.
Entity IDs and hidden tags continue to constrain the selected history scope.

The trusted startup file is a `typed-payload-v1` Grid with empty metadata,
exact columns `id`, `ts`, `val` (empty column metadata), and exactly those three
members in each row. `id` is a Ref without display, `ts` is a DateTime and `val`
uses the admitted point schema. The entity dataset declares each point's
`his`, `kind`, `tz` and Number `unit`. For example, one Bool sample is:

```json
{"version":1,"value":{"kind":"grid","meta":{},"cols":[{"name":"id","meta":{}},{"name":"ts","meta":{}},{"name":"val","meta":{}}],"rows":[{"id":{"kind":"ref","value":"switch-a","display":null},"ts":{"kind":"dateTime","seconds":"1730611800","nanos":123456789,"offset":0,"timezone":"UTC"},"val":{"kind":"bool","value":true}}]}}
```

The file is relative to its config or absolute, regular UTF-8, at most 4 MiB,
10,000 rows, depth eight and 100,000 typed nodes. Each point's input timestamps
must be strictly increasing and use consistent named-zone offsets. Unsupported
schemas/values, duplicate timestamps and malformed files fail before serving or
HTTP readiness, with generic diagnostics. Loading seeds one explicitly owned,
bounded memory provider; it is startup initialization, not an MCP write action.
The CLI uses existing finite `HistoryLimits` and `ReadLimits`; request controls
only tighten them. Embeddings may select other providers under the same shared
provider and lifecycle contract.

## Qualification

```sh
uv run --project scripts/schema-client --frozen python scripts/mcp/qualify.py
uv run --project scripts/schema-client --frozen python scripts/mcp/qualify_http.py --features mcp,mcp-http
```

Without `--binary`, these commands install a fresh temporary CLI and remove it
afterward. The independent history helper validates schemas, actual scalar and
control representations, full selected discovery/results at the accepted floor,
an exact 16-row prefix of 130 retained Bool samples, complete Number/Str ranges,
DST/fraction/Number/NA/gap semantics, partial/empty/denied results,
explicit History permission and unusable byte caps. Mutation controls reject
missing/duplicate/reordered samples, wrong identity/unit/fraction/terminal and
NA-to-null substitution. Native transport tests gate provider open/pull/close
and verify cancellation, disconnect, deadlines, owner stop and actual release.
These local fixtures do not qualify external models, agent hosts, production
identity infrastructure, public deployment, snapshots or resumed history reads.
