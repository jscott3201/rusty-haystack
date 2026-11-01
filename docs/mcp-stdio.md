# Read-only MCP over stdio

The optional `rusty-haystack-mcp` crate exposes two entity tools over the managed
application read service: `haystack_query` and `haystack_read`. Select its `stdio`
feature explicitly. The standalone CLI selects it with `--features mcp`; ordinary
core, application and CLI builds do not activate MCP. PostgreSQL, Python bindings
and an HTTP listener are unnecessary for this profile. The independent
[authenticated HTTP profile](mcp-http.md) uses the `http`/CLI `mcp-http` feature.

The adapter uses official `rmcp = 3.8.0`, with default features disabled and only
its server support selected in production. It supports exactly MCP `2026-07-28`
discovery and `2025-11-25` initialization. Client metadata negotiates protocol
behavior; it cannot select a principal, scope, dataset, path, URL or command.
Explicitly selecting a history service adds the bounded
[`haystack_history` tool](mcp-history.md). Explicit semantic selection adds
[H4 definition/library resources](mcp-semantic-resources.md), independently of
tool output presentation. No mutation, command, prompt, sampling, elicitation
or task capability is exposed.

## Standalone launch

```sh
cargo +1.99.1 install --locked --path haystack-cli --features mcp
haystack mcp stdio --config /absolute/path/mcp.toml
```

The command requires **Unix anonymous/FIFO pipes on both stdin and stdout**.
An agent host starts it with pipes. Running it directly in a terminal, redirecting
stdin from a regular file or `/dev/null`, or selecting an unsupported platform
fails with an explicit diagnostic. The selected runtime profile is Unix; Windows
stdio behavior is not qualified. Safe Tokio pipe conversion verifies descriptor
type and access before using cancellable nonblocking I/O.

Create an explicit version 1 configuration:

```toml
version = 1
dataset_file = "entities.trio"
dataset_format = "trio"
principal = "local-building-reader"
entity_ids = ["site-a", "ahu-a"]
hidden_tags = ["privateNote"]
request_timeout_ms = 10000
max_response_bytes = 1048576
```

`dataset_file` is relative to the configuration file, or absolute. Formats are
`trio`, `zinc`, Haystack v4 `json`, and the explicit project-owned
`typed-payload-v1` Grid. Configuration is limited to 64 KiB and the
dataset to 4 MiB/10,000 rows. A conservative depth limit precedes dataset decoding
and also counts delimiter characters inside strings. Both path metadata and the
opened descriptor must identify regular UTF-8 files. The Unix open uses
`O_NONBLOCK` so replacing a checked path with a FIFO cannot wait for a writer
before descriptor rejection. Unknown fields, versions and formats fail closed without echoing file
contents or parser diagnostics. No ambient dataset or credentials are loaded.

The launch principal is a trusted embedding identity, not a client-provided login.
The immutable policy permits entity reads for the explicit ID set, masks
`hidden_tags`, and removes references to entities outside that set. Up to 512
nonempty IDs and hidden tag names are accepted; individual names and the principal
are limited to 256 bytes. An empty entity scope is rejected. File contents and
entity values remain untrusted data even when returned to a model.

Output defaults to `haystack-h4-json-v1`. The optional top-level
`output_profile = "haystack-jeto-v1"` and `nominal_specs` select the
[rich-value profile](mcp-rich-values.md), with fixed boxed schemas and a required
profile acknowledgment on every tool call. Dataset format does not select output.

For history, select a bounded `history_file` and explicitly grant
`history_read = true`; see [the history profile](mcp-history.md). Without that
provider selection, discovery and the existing two-tool launch behavior are unchanged.

Example host command array:

```json
["/absolute/path/haystack", "mcp", "stdio", "--config", "/absolute/path/mcp.toml"]
```

Stdout contains newline-delimited JSON-RPC only. Diagnostics and the successful
joined-cleanup receipt go to stderr. Feature-disabled builds retain help and a
clear installation error.

## Tools and result profile

The entity tools publish fixed input/output schemas and read-only, non-destructive,
idempotent, closed-world annotations. Annotations are discovery hints; the shared
policy enforces access. Each takes optional `projection` (tag names), `page_size`
and `cursor`. `haystack_query` requires a Haystack `filter` string;
`haystack_read` requires a nonempty `ids` array of reference values without `@`.
Unknown arguments are rejected. For example:

```json
{"name":"haystack_query","arguments":{"filter":"site","page_size":20,"projection":["id","dis"]}}
```

Success is an object with `profile: "haystack-h4-json-v1"`, `ok: true`, the actual
shared H4 JSON codec `grid`, `complete`, nullable `cursor`, and authorized
`row_count`. Text content is the same serialized object as `structuredContent`.
The envelope does not invent an atomic dataset or catalog observation. H4 values
retain the selected codec's units, timestamps, reference, null and missing
semantics. Rich typed `Int`, `Float`, `Buf`, `Nominal` and `None` values fail strict
projection recursively, including inside containers; projection may exclude an
unsupported tag. They are never silently coerced.

An empty authorized result succeeds with zero rows. Missing and denied IDs are
indistinguishable and expose no candidate or denied counts. Tool execution errors
set `isError: true` and return the same profile with `ok: false` and
`error.code`. Codes distinguish `invalid_query`, `unavailable`, `forbidden`,
`stale_cursor`, `capacity`, `budget`, `output_budget`, `projection`, `deadline`,
`cancelled`, `not_ready` and `closed`; shared commit-unit errors retain
`unit_too_large`. Unknown tools, malformed arguments and unavailable methods use
JSON-RPC errors: malformed JSON uses `-32700`, invalid request envelopes use
`-32600`, invalid selected-method parameters use `-32602`, and unavailable
methods use `-32601`. Valid request IDs are echoed; an unidentified error omits
the ID, as required by both selected MCP schemas. An integral float-form ID
(`"id":1.0`) is valid and answered as the integer `1`, as rmcp has read it since
3.6.0; non-integral (`1.5`), out-of-range, boolean, null, object and array IDs
remain invalid. No error includes raw query, entity, file or principal contents.

Continue with the same tool, query/IDs, projection and page size plus the returned
cursor. Every page captures current policy and runs shared cursor validation.
Scope, principal, query, projection, page size, graph or catalog changes can make
a continuation stale; restart rather than treating it as a stable snapshot.

## Bounds and lifecycle

The default adapter limits are 64 KiB per input frame, JSON depth 32, 128 bytes per
string request ID, 16 admitted requests, 16 pending responses, 1 MiB per complete
JSON-RPC output frame and a 10-second absolute request deadline. Trusted embedding
can select finite limits within validated maxima; the CLI exposes response bytes
and timeout (10–60,000 ms). The response range is 8 KiB–16 MiB for H4 and
16 KiB–16 MiB for Jeto. The profile-specific minimum allows both discovery schemas
and a maximally escaped 128-byte request ID; smaller configurations fail before
serving. Shared read budgets independently bound query parsing,
IDs, rows, graph work, retained bytes, values, cursor storage and duration. Its
default duration is five seconds, which may be tighter than transport delivery.
Selecting history raises the default and minimum full response budget to 2 MiB
for either output profile; the maximum remains 16 MiB. This covers the selected
three-tool inventory and shared history encoding headroom. Per-call history
data limits and diagnostic limits are described in [the history profile](mcp-history.md).
Selecting semantic resources adds a 128 KiB floor, detailed in the
[resource profile](mcp-semantic-resources.md); a larger history floor still applies.

Frame collection is bounded before JSON materialization. The reader independently
processes cancellation while execution or output is busy. Admission and response
queues never grow beyond their configured quotas. Completion notifications and
timer wakeups recheck the live request and partial-frame deadlines, so a finished
request cannot expire an idle connection. The full output bound counts
JSON-RPC framing, escaping, and duplicated text plus structured content. A
conservative reservation can reject a shared grid before it reaches the physical
frame cap. A partial frame, blocked delivery or unfinished request expires its
absolute deadline and closes the connection with failure.

Cancellation before the first successfully written byte suppresses the original
response, including when the writer has already returned Pending. If delivery has
already begun, cancellation promptly closes the connection with failure because
a partial JSON-RPC frame cannot leave a reusable stream. The request's ID, transport quota
and application registration remain held until its shared worker actually exits
and delivery/suppression completes. Duplicate live IDs are rejected; reuse is
permitted only after release. The shared `ReadAdmission::read_joined` seam waits
for the worker's actual permit and registration release after cancellation. The
existing prompt-cancellation `read` API retains its original behavior.

`haystack_mcp::serve` accepts an explicit `StdioConfig` containing limits,
output profile and semantic-resource selection, and borrows an `ApplicationHandle`
and caller runtime. Awaiting
its result joins reader/writer tasks and all request leases; closing the adapter
leaves the application available to other adapters. The supplied I/O must be
cancellation safe, and trusted application callbacks must honor the shared
nonblocking callback contract. A stalled borrowed runtime is not converted into
a false cleanup receipt. Dropping the serving future is not a join receipt.

The CLI owns its application and runtime. EOF or SIGINT/SIGTERM with stdin still
open stops the adapter, then awaits application close and actual termination
before dropping the runtime outside async. A clean stop remains successful when
it interrupts a blocked response; interrupted bytes are not counted as a delivered
response. An independently observed failure remains a failure. Broken output, framing violations,
capacity failure and deadline failures exit nonzero after cleanup. No blocking
Tokio stdin worker can hold runtime shutdown open.

## Qualification

Run focused adapter and lifecycle tests:

```sh
cargo +1.99.1 nextest run --locked -p rusty-haystack-mcp --features stdio
cargo +1.99.1 nextest run --locked -p rusty-haystack-app -E 'test(joined_read_waits)'
uv run --locked --project scripts/schema-client --python 3.12 python scripts/mcp/qualify.py
```

The final command installs into a temporary consumer directory, uses the existing
locked independent JSON Schema validator, and observes real pipe subprocesses.
It also launches the opt-in rich profile and independently decodes its selected
typed values; see [rich qualification](mcp-rich-values.md#qualification).
It checks discovery, schemas, useful scoped reads, argument rejection, stdout
purity, EOF, signals with stdin open, broken output, backpressure, non-pipe launch,
sanitized configuration errors, FIFO launch rejection, and graceful EOF/signals
while an undrained partial response fills the output pipe. `--binary PATH` is an
explicit quicker mode and does not constitute a fresh installation. CI runs the selected adapter feature
tests and the installed fixture on Linux; local execution supplies macOS evidence.
