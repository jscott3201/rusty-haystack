# Shared state subscriptions

`StateSubscriptionService` owns bounded entity watches for native callers,
HTTP and WebSocket attachments. A scoped watch survives transport disconnects
within its lease and authentication session. It reports current authorized
state coalesced from complete retained entity commits; it is not an audit stream
of intermediate values or a durable/distributed subscription service.

## Application selection

Select the owner with `ApplicationBuilder::state_subscriptions`. Construct it
from the builder's exact managed `ReadService`, an `EphemeralMutationStore`, and
`SubscriptionLimits`. If entity mutations are also selected, both services must
share the **same store handle**, including when selection order is reversed.
Another store on the same graph is a different entity authority and is rejected.
Subscriptions do not require selecting entity writes. History reads and writes
retain their independent provider, receipt authority and lifecycle.

The standalone legacy `HaystackServer::start()` convenience owner selects this
service automatically. Embeddings using `with_application` or
`with_scoped_reads` select it on their `ApplicationBuilder` before starting the
application or publishing its router. Both profiles advertise and route `ws`
and the watch operations only when that owner is selected; an application
without it has no watch endpoints. There is no separate router-owned watch
registry.

```rust
use std::sync::Arc;
use haystack_app::{
    AllowAll, ApplicationBuilder, EphemeralMutationStore, ReadLimits,
    StateSubscriptionService, SubscriptionLimits,
};
use haystack_core::graph::{EntityGraph, SharedGraph};

let graph = SharedGraph::new(EntityGraph::new());
let builder = ApplicationBuilder::new(
    graph.clone(), Arc::new(AllowAll), ReadLimits::default(),
)?;
let store = EphemeralMutationStore::new(graph);
let subscriptions = StateSubscriptionService::new(
    builder.handle().read_service(), store, SubscriptionLimits::default(),
)?;
let builder = builder.state_subscriptions(subscriptions)?;
# Ok::<(), haystack_app::ReadError>(())
```

`AllowAll` is an explicit trusted/compatibility policy. A scoped deployment
supplies a versioned immutable `ReadPolicy`; it must authorize
`ReadOperation::Subscriptions`, ordinary entity reads, and every exposed
entity, tag, reference and nominal value. Policies must change `scope_key` when
authority changes. Retained projections are also reauthorized before replay,
acknowledgement or membership replacement. Denied identities are never sent in
removal notifications; authority loss produces a non-identifying resync.

The application starts one maintenance task and waits for its destruction on
close. Idle watches retain bounded records, not ordinary read permits. Each
active operation acquires a fresh read admission for source validation, graph
capture, execution and encoding. Application close seals publication and drains
admitted work, WebSocket writers and maintenance before cleanup completes.

## Session authority and creation recovery

Scoped operations require a `SubscriptionSession`. The HTTP authentication
manager creates this noncredential handle under its bearer-issuance lock and
injects it only after token validation. The fixed expiry and shared cancellation
signal are retained by open sockets; logout cancels the session before removing
the bearer. Expiry and revocation do not depend on further HTTP traffic. A new
login with the same username is a new session and cannot resume an old scoped
watch. Watch records contain no bearer credentials.

Trusted embeddings explicitly mint `SubscriptionSession::trusted(subject,
lifetime)` and use its principal in `ReadContext`. Clones share authority;
`close()` revokes all clones. `authenticated(principal, absolute_expiry)` is a
trusted authentication integration seam, not a request-body authentication API.
A context with another principal or permission set cannot reuse a session.
Anonymous callers are supported only by the legacy profile below.

First call `SubscriptionRequest::Describe` to obtain the application's
subscription authority and entity dataset. Create with a caller-known opaque
key, that authority, the original ordered entity ID list and a requested lease.
The key binds the exact original intent, including order and duplicates, before
membership normalization. A changed intent with the same key is a conflict.
An in-progress matching create can report `Pending`; an explicit later retry
resolves the same watch. The client never makes that retry automatically.

Creation bindings and terminal tombstones remain for the valid session lifetime.
After unsubscribe or terminal resync, replaying a create cannot revive its key.
Capacity is reserved before the initial snapshot; an inadmissible new watch has
no published record. Failed initial capture releases its reservation. Recreating
the owner changes its authority even if the entity store is retained, so an old
creation request cannot silently create another watch after authority loss.

## Prepared deliveries and acknowledgement

The initial projection, graph incarnation, catalog generation and feed head are
captured under one graph read guard. Publication compares graph state and the
watch revision. Every subsequent operation traverses retained complete commit
units immediately; notifications are hints and are not needed to resume.

A watch holds at most one immutable prepared delivery. The delivery includes:

| Field | Meaning |
| --- | --- |
| `watch` | Subscription authority, opaque watch identity and caller creation key |
| `dataset`, `incarnation`, `catalogGeneration` | Captured entity authority and catalog |
| `scopeGeneration` | Membership generation |
| `token` | Identity of this exact prepared delivery |
| `from`, `through` | Acknowledged baseline and captured complete feed head |
| `initial` | Full initial state for this membership generation |
| `rows`, `removed` | Complete changed records and authorized removed identities |

An initial delivery has `from == through` and no removals. Replace the local
state with its rows. Later deliveries replace the listed complete records and
remove the listed IDs. Commits that leave the watched projection unchanged may
produce an empty delta carrying an advanced feed head; acknowledge it normally.

Preparing, returning, retrying or sending a delivery **does not acknowledge it
or renew its lease**. Until explicit acknowledgement, a valid retry returns the
same token, fences and payload after current reauthorization. New graph changes
remain behind that prepared delivery and are caught up after it is acknowledged.

| Request | Behavior |
| --- | --- |
| `Poll` | Replay the prepared delivery, prepare the next bounded delta, or return `Idle` |
| `Resume` | Same operation with the caller's expected scope and acknowledged fence; mismatch returns resync |
| `Acknowledge` | Advance only for the exact scope, token and `through`; duplicate last ACK is idempotent |
| `Replace` | Authorize new membership and publish a new scope with a fenced initial snapshot; an empty set is allowed |
| `Renew` | Explicitly restart the lease from this operation; it does not acknowledge delivery |
| `Unsubscribe` | Close and retain the terminal binding; duplicate close is idempotent |

An old, future or foreign ACK cannot advance a new prepared delivery or scope.
A duplicate previous ACK while a newer delivery is pending leaves that newer
delivery intact. A lost membership response can be reconciled by polling;
blindly repeating its stale expected generation does not apply it twice.

Lease expiry, feed gaps, incarnation/catalog/policy changes, invalid scope,
shutdown and bounded-work overflow have explicit resync reasons. A retained
commit unit is never split to fit a poll budget. Session expiry/revocation also
invalidates watches and frees their bindings. `Unknown` means the response
cannot establish whether an operation took effect. Preserve the caller-known
key or watch identity and decide explicitly how to reconcile it.

## HTTP and WebSocket profiles

The scoped HTTP operations are `watchInfo`, `watchSub`, `watchPoll`, `watchAck`,
`watchRenew` and `watchUnsub`. Each request/response is one H4 grid with exactly
one `payload` STR cell holding the bounded `state-subscription-v1` typed envelope.
Supported outer codecs are Zinc, JSON v3 and JSON v4. Strict field sets,
canonical decimal control strings, exact identities and response correlation
are checked before a client accepts an outcome. This is an H4 extension; it does
not add a Haystack 5 wire protocol.

The scoped WebSocket profile requires explicit negotiation of
`haystack.state-subscription.v1`. Each JSON text envelope has exactly
`profile`, `reqId` and `payload`; `profile` is `state-subscription-v1`, `reqId`
is a canonical unsigned decimal string, and `payload` contains the same typed
DTO used by HTTP. Operations are request/response; callers explicitly poll or
resume. The scoped profile does not automatically push, ACK, renew, reattach or
resubscribe. It does not fall back to the legacy profile.

There are eight queued commands per server socket. Commands are executed with
fresh admission after sink capacity becomes available; previously authorized
payloads are not stored in the outbound queue. Session closure and application
closure interrupt both queued and active writer work. An overflow terminates the
connection with close code 1013 and, when the peer can receive it, a non-identifying
`Resync(Overflow)` envelope with reserved `reqId: "0"`. An unreadable terminal
response remains unknown to the client. No ACK fence advances on queue overflow.
Individual scoped frames/reassembled messages are bounded by the 6 MiB outer
wire ceiling and the typed payload by 1 MiB; application limits are usually
smaller. Writes and sink-capacity waits have a two-second bound.

Use `HaystackClient::state_subscription` on a first-party HTTP transport or on
its attached `SubscriptionWsTransport`. The explicit `SubscriptionTransport`
trait is also available to custom transports that enforce complete bounded
responses and at-most-once dispatch.

```rust,ignore
let http = HaystackClient::connect(api_url, username, password).await?;
let ws = http.attach_subscription_ws(websocket_url).await?;
let outcome = ws.state_subscription(&saved_resume_request).await?;
```

Attachment reuses the HTTP client's private bearer only for the same validated
host, effective port and API path, with `http`/`ws` or `https`/`wss` pairing.
Basic authentication, userinfo, query strings, fragments and authority changes
are rejected before dialing. WebSocket TLS uses public roots; it does **not**
inherit custom reqwest CA roots, client certificates or insecure verification
settings. The existing `connect_ws` helper performs another login for the
legacy profile and does not resume the HTTP client's scoped session.

The first-party clients never retry an uncertain create, ACK, renewal or other
control automatically, and never follow HTTP redirects for this profile.
Malformed, mismatched or unreadable post-dispatch responses become `Unknown`.
The scoped WebSocket client serializes calls with eight bounded callers;
cancelling a dispatched call drops its socket so an uncertain partial frame is
never continued by a later call.

## Bounds and the legacy profile

Default application ceilings are 128 active watches, 32 per session, 1,024
creation bindings, 128 bindings per session, 512 IDs per watch, a 64 KiB retained
projection and 256 KiB prepared typed delivery. A conservative reservation is
charged against a 128 MiB aggregate retained-state budget before creation.
These independent limits may admit fewer watches than the count ceiling.
The default maximum lease is five minutes; the wire ceiling is one hour.
Maintenance runs every 100 ms. Read-service input, work, value, output and
concurrency limits apply to active operations as well.

Legacy HTTP/watch and JSON v3 WebSocket calls use this same application owner.
`HaystackServer::start` selects that owner for the compatibility application.
Legacy anonymous calls remain possible when authentication is disabled. This
profile keeps coarse principal ownership and acknowledges automatically as part
of polling; it offers no scoped creation key, exact-session resume, explicit
ACK fence or durable recovery promise. Legacy watches use the configured maximum
lease and must be subscribed again after expiry. WebSocket disconnect cleans up
only watches created by that connection. Other connections' and HTTP-created
watches are preserved, including for the same username.

Legacy pushes are fresh authorized current-state views and do not consume the
polling fence. The legacy request/response frame contract and selective-ID
removal remain documented in [the server API](server-api.md). The application
owner's limits apply in addition to that frame grammar. Python watch bindings,
portable durable storage and distributed subscription ownership are outside
this slice.
