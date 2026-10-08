# Scoped history mutation and receipts

`haystack-app::HistoryMutationService` authorizes one point's bounded sample
batch and publishes its samples, retention state, point generation, required
history change record and operation receipt together. Select it explicitly over
the exact `HistoryService` already selected on the application. Read or entity
mutation permission does not imply history write permission.

```rust,no_run
use std::sync::Arc;
use haystack_app::{AllowAllHistoryMutations, HisStore, HistoryLimits,
    HistoryService, HistoryMutationLimits, HistoryMutationService};
# fn select(builder: haystack_app::ApplicationBuilder) -> Result<haystack_app::ApplicationBuilder, haystack_app::ReadError> {
let history = HistoryService::new(builder.handle().read_service(),
    Arc::new(HisStore::new()), HistoryLimits::default())?;
let writes = HistoryMutationService::new(history.clone(),
    Arc::new(AllowAllHistoryMutations), HistoryMutationLimits::default())?;
let builder = builder.owned_history(history)?.history_mutations(writes)?;
# Ok(builder)
# }
```

`AllowAllHistoryMutations` is a deliberate trusted embedding option. Applications
normally implement immutable `HistoryMutationPolicy` rules for the principal,
point, original samples and configured schema. Reconciliation receives the
retained original request even after a point is removed. Rules must be quick,
nonblocking and free of side effects. `replace_policy` atomically replaces the
rules; delayed plans from the previous generation reject at publication.

The provider explicitly opts into `history_write_capability`, identifying the
same `HisStore` used by reads and every native writer. Absence of that capability
is read-only. Capability is checked before new mutation preparation. Provider
initialization and close run once through the existing history owner; selecting
writes adds no second resource lifetime. Independently constructed read services,
even around the same store, cannot replace the selected application history.

## Original intent and outcomes

A `HistoryWriteRequest` supplies an operation identity, expected generation and
ordered `HistorySample { ts: HDateTime, val: Kind }` values. Obtain authoritative
history state from an authorized bounded read. The identity contains the history
authority, point ID, point-history incarnation and caller operation ID. The
binding also includes stable principal category and subject. Changing permission
lists never creates a new operation namespace. Entity and history operations
have distinct DTOs, stores and endpoints; the same operation ID text can be used
independently in both domains.

Canonical typed bytes preserve every original sample, including duplicate order,
DateTime offset and exact timezone spelling, Number bits, unit absence and exact
unit spelling. Expected generation belongs to this original intent, not the
identity key. An authorized identical submission returns the retained outcome
before current point/schema/generation admission. A changed payload or expected
generation under a recognized ID conflicts.

| Outcome | Meaning |
| --- | --- |
| `Committed` | One generation advance, a history change sequence, sample/retention counts and an explicit qualification |
| `Rejected` | This submission attempt caused no sample, generation or history change effect |
| `Unknown` | Effect is unresolved; preserve the identity and explicitly look up its receipt |

A validly admitted empty scoped batch retains `Rejected(EmptyBatch)` against its
original canonical intent. Repeating it returns that rejection; changing it to
a nonempty batch under the same ID conflicts. It publishes no samples,
generation or history change. Malformed, unauthorized, unsupported and stale
requests rejected before receipt reservation create no new binding. Once a
provider receives a reserved plan, guaranteed rejection remains bound. Dropping
a plan leaves `Unknown(Pending)`; it does not authorize replay.

Receipt count and conservative retained-byte limits belong to the authority.
New identities fail before sample effects when capacity is exhausted. Recognized
records are not evicted to make an ID reusable. Lookup always applies current
reconciliation policy to the original retained intent. Missing receipts and
requests addressed to a fresh authority return `Unknown(Missing)`, including
after ephemeral state loss; absence never proves non-effect.

## Admission and publication

The point must declare `his`, a supported Bool/Number/Str `kind`, and a supported
Haystack short-name `tz`. Every Number point requires registered unit metadata.
An original sample must have the matching Kind, or NA as the explicit error
sample. Number samples may be unitless or resolve to the same registered
`Unit.name` as the point. For example, `°F`/`fahrenheit` and `USD`/`$` are aliases;
no quantity conversion or unit spelling normalization occurs.

Missing `val`, Null, None, Remove, native Int/Float and rich kinds are rejected.
Null rejection is this single-point profile's restriction, not a universal H4
rule for sparse multipoint history. Canonical unitless `f64::NAN` is supported;
other NaN payload/sign bits and every unit-bearing NaN are unsupported. Finite
bits and infinities retain Number semantics; an infinity's explicit unit follows
the ordinary identity rule. Zinc special Number decoding preserves units before
admission, including on rejected NaNs. JSON v4 non-string units fail decoding.

Each submitted `HDateTime.tz_name` must exactly match the configured spelling.
`GMT`/`UTC`, `Calcutta`/`Kolkata` and short/full IANA forms are not equated. The
submitted offset must match the locked timezone rules at that timestamp. Both
explicitly valid offsets in a DST overlap are admitted; a nonexistent local time
or inconsistent offset rejects. Leap-second fractions retain their original
nanoseconds. Historical offsets containing seconds cannot be represented by the
selected H4 wire forms and reject before dispatch or scoped admission; native
typed timestamps and trusted store values are preserved. `Rel` and full IANA
spellings remain unsupported in this H4 profile. This is distinct from read ranges, whose explicit boundaries
may use another supported zone. No global timezone codec normalization is added.

Samples are upserts ordered by instant. Original duplicate timestamps use the
last input value. Retention evicts the oldest samples and reports that loss. Each
accepted nonempty batch advances the point generation exactly once, even if the
values equal existing samples. One point's batch is the atomic unit. Multipoint,
combined entity/history transactions and equipment commands are unsupported.

Preparation checks incoming rows/bytes, existing-series source sizes, sorting
and merge work, retained memory and one absolute deadline before publication.
Submission and receipt lookup reserve principal/binding hashing, copying and
outcome storage before those operations. Existing-series discovery admits each
row's conservative work and source bytes before value/unit validation, then
reserves clone and merge work separately.
Limits are conservative source-size budgets, not allocator measurements; a small
request can fail when the existing series is too large to prepare within bounds.
The default mutation limits allow up to 128 incoming samples, 65,536 source bytes,
100,000 existing samples and 8 MiB of prepared source. Shared `ReadLimits` apply
as well, so a lower byte/work limit can stop a request before its row ceiling.
No batch is silently split or partially accepted.

The authority reserves receipt capacity and prebuilds the series, retention,
change record and receipt before provider dispatch. Publication holds the current
policy generation and graph read ownership while acquiring authority and point
ownership. Its lock order is policy, graph read, history authority, point. Failed
try-locks release graph/publication ownership before waiting. There are no policy
callbacks, provider waits or notifications inside publication locks. A changed
graph observation, point generation/incarnation, policy, deadline, cancellation
or sealed owner prevents publication. This closes schema admission races without
claiming an atomic graph/history snapshot or cross-store transaction.

The original admission/work lease survives body decoding, provider execution,
response construction and any retained prepared plan. A public timeout or
`Unknown` result does not mean the worker stopped. Shutdown waits for actual
completion; a provider retaining a plan can cause an explicit shutdown timeout.
A retained plan rejects after the owner seals, and releases ownership only when
published, rejected or dropped.

## Memory authority and resets

`HisStore` retains point identities, series, generation, receipts and the bounded
history change ledger together. A missing series gets a stable generation-zero
identity when first observed, with bounded point count and ID allocation. Default
limits are 4,096 points, one million samples per point, 1,024 receipts, 64 MiB of
receipt accounting and 4,096 retained history change records. `HistoryStoreLimits`
allows smaller bounded configurations. Exhaustion fails explicitly.

Trusted native nonempty writes use the same authority and advance that point's
generation and change ledger once. Native empty writes remain no-effect and do
not allocate a missing point. `reset_point` is an explicit trusted history reset:
it replaces the series under the same point handle with a new incarnation and
generation zero, preserving old receipts. Old sessions and plans cannot revive
retired state. Graph replacement does not reset history; history reset does not
replace the graph. A newly constructed store has a new history authority.

The history change ledger has finite retention. A receipt acknowledges atomic
publication of its required record, not indefinite record retention.
`retained_changes` is trusted in-process inspection for fixtures; this card adds
no public history streaming or cursor API.

All selected storage is **ephemeral process memory**. Recreating a service around
a surviving authority proves reconciliation of retained process state.
`ProviderProtocol` fixtures prove failure, ordering and acknowledgement behavior.
Neither qualification proves disk atomicity, process-crash recovery, database
COMMIT semantics or multiprocess operation. Durable backend qualification remains
separate work.

## HTTP and Rust client

Scoped discovery advertises `hisWrite` and `hisReceipt` only when history writes
were selected when routing was built. `hisWrite` requires coarse `write`
permission and the application policy. `hisReceipt` requires coarse `read` and
current reconciliation policy; ordinary read permission grants no receipt
access by itself.

The shared `haystack_core::codecs::history_mutation` adapters put a versioned
`history-write-v1` control in `historyWrite` STR grid metadata. Submissions retain
ordinary `ts`/`val` rows and an `id` Ref that must match the control identity.
Lookup and result grids carry their typed control without data rows. Unknown
fields/versions and noncanonical counters reject. All public generations and
counts use exact decimal strings, including values above 2^53. Zinc and JSON
v3/v4 are supported. Scoped Zinc rows require complete scalar consumption and
reject surplus cells; legacy Zinc grid handling remains separate.

A normal result grid carries committed/rejected/unknown data. A valid typed
outcome is not discarded because an `err` marker is also present. Empty legacy
acknowledgements never establish scoped success. Authentication, malformed
control, negotiation and body-limit failures may use HTTP errors.

`HaystackClient::his_write_scoped` makes one submission on an explicitly opted-in
`HistoryMutationTransport`. `reconcile_history` performs a separate lookup.
Response-body failures, malformed or foreign identities, contradictory committed
counts and empty acknowledgements preserve the original identity as `Unknown`.
The helper never automatically resubmits. First-party HTTP transports disable
retries and redirects, bound complete receipt bodies and check response format.
Unqualified caller-supplied reqwest clients fail before dispatch. Custom transports
own that same contract. WebSocket has no history mutation extension.

The existing Rust/Python `his_write` helpers remain explicit legacy grid calls.
They supply no operation control and cannot bypass scoped admission. Python
scoped receipt wrappers are not introduced by this slice.
