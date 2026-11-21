# rusty-haystack

Fast Python bindings for [Project Haystack](https://project-haystack.org) powered by Rust.

Provides types, codecs (Zinc/Trio/JSON/JSON v3/CSV), filters, an in-memory entity graph, ontology validation, and Xeto support — all backed by a native Rust core for speed and correctness.

## Installation

```sh
pip install rusty-haystack
```

Requires Python 3.11+. Pre-built wheels are available for Linux and macOS (x86_64 and aarch64).

## Quick Start

```python
import rusty_haystack as rh

# Create an entity
entity = rh.HDict({
    "id": rh.Ref("site-1", "Demo Site"),
    "site": rh.Marker(),
    "dis": "Demo Site",
    "area": rh.Number(5000, "ft²"),
    "geoCoord": rh.Coord(40.7128, -74.0060),
})

# Build a graph and query it
graph = rh.EntityGraph()
graph.add(entity)
results = graph.read("site")

# Encode/decode across formats
zinc_str = rh.encode_grid("text/zinc", results)
json_str = rh.encode_grid("application/json", results)
decoded = rh.decode_grid("text/zinc", zinc_str)

# Validate against the standard ontology
ns = rh.DefNamespace.load_standard()
issues = ns.validate_entity(entity)
```

## Features

- **Kind types** — Marker, NA, Remove, Number (with units), Ref, Uri, Symbol, XStr, Coord, HDateTime, HDict, HGrid, HList, plus exact Int, Float, NoneValue, Buf and Nominal wrappers
- **Codecs** — Zinc, Trio, JSON, JSON v3, CSV encoding and decoding
- **Filters** — Parse and evaluate Haystack filter expressions against entities
- **EntityGraph** — In-memory graph with CRUD, filter queries, and ref traversal
- **Ontology** — DefNamespace with `is_a`, `fits`, `validate_entity`, subtype/supertype queries
- **Xeto** — Load/unload Xeto libraries, inspect specs and slots, export to Xeto source

## Codecs

Supported MIME types: `"text/zinc"`, `"text/trio"`, `"application/json"`, `"application/json;v=3"`, `"text/csv"`.

```python
# Grid encode/decode
zinc_str = rh.encode_grid("text/zinc", grid)
grid = rh.decode_grid("text/zinc", zinc_str)

# Scalar encode/decode
s = rh.encode_scalar("text/zinc", rh.Number(72.5, "°F"))
v = rh.decode_scalar("text/zinc", s)

# Project-owned typed payload v1 preserves rich variants and exact float bits.
typed = rh.HDict({"count": rh.Int(2**53 + 1), "sample": rh.Float.from_bits(1)})
payload = rh.encode_typed_value(typed)
assert rh.decode_typed_value(payload) == typed
```

Plain Python integers still become Number, but must fit signed 64-bit and be
exactly representable as binary64; use Int when retaining integer identity or
precision matters. Number and Coord are immutable and use representation
equality, including distinct signed zero. H4 codecs reject rich values recursively.

HDateTime retains exact nanoseconds with a fixed offset and opaque timezone label.
Its `dt()` method rejects precision loss, leap seconds and out-of-range local
calendars. Calendar components and offsets accept concrete integers, including
bool, without invoking arbitrary numeric hooks. Native date/time conversion reads
stored fields without invoking subclass properties; time values require no
timezone and `fold=0`. Runtime checks remain
necessary for numeric ranges and temporal restrictions even with static typing.
The package ships root and submodule stubs for Python 3.11+.

## EntityGraph

```python
graph = rh.EntityGraph()
graph.add(rh.HDict({"id": rh.Ref("site-1"), "site": rh.Marker(), "dis": "HQ"}))
graph.add(rh.HDict({"id": rh.Ref("ahu-1"), "equip": rh.Marker(), "siteRef": rh.Ref("site-1")}))

results = graph.read("equip")       # filter query
graph.refs_from("ahu-1")            # -> ["site-1"]
graph.refs_to("site-1")             # -> ["ahu-1"]
```

## Ontology & Xeto

```python
ns = rh.DefNamespace.load_standard()

ns.is_a("ahu", "equip")            # True
ns.fits(entity, "site")            # True
ns.subtypes("equip")               # ["ahu", "vav", ...]
ns.validate_entity(entity)         # list of issue strings

# Load custom Xeto libraries
ns.load_xeto(source_text, "myLib")
spec = ns.get_spec("myLib::MyType")
```

## License

MIT

## Async HTTP client

`AsyncHaystackClient` owns an HTTP/SCRAM session and its native I/O resources. Its
constructor stores configuration without opening a connection or requiring an
asyncio loop. The first operation binds the exact running loop and OS thread;
using the owner from another loop or thread raises `RuntimeError`, even after
close. Create a fresh owner for another loop or after a terminal failure.

```python
import asyncio
from rusty_haystack import AsyncHaystackClient
from rusty_haystack.client import TlsConfig

async def read_sites(url: str, username: str, password: str):
    client = AsyncHaystackClient(url, username, password, timeout=30.0)
    async with client:
        deadline = asyncio.get_running_loop().time() + 5.0
        return await client.read("site", limit=100, deadline=deadline)
```

You can instead `await client.connect()` and use `await client.close()` in a
`finally` block. Keep the owner before connecting so failed or cancelled connect
can still be observed through `close`. Failed or cancelled connect starts
terminal cleanup. A cancelled read cancels that request; the open client remains
usable once its admission slot has drained.

`connect`, `read`, `read_by_id`, `read_by_ids`, `close`, `__aenter__` and `__aexit__` return asyncio **Futures**.
Direct `await`, `asyncio.ensure_future`, `gather` and `wait_for` accept them. APIs
requiring coroutine objects, including `create_task` and `TaskGroup.create_task`,
need an explicit wrapper:

```python
async def read_one(client):
    return await client.read("site", limit=1)

# Inside a running loop:
# task = asyncio.create_task(read_one(client))
```

The default operation budget is 30 seconds. Constructor `timeout` must be positive,
finite and representable. An optional absolute `deadline` uses the bound loop's
`time()` clock and can **shorten** that budget. The clock is converted once at the
method call, so queueing, requests and body collection share one deadline. Deadline
expiry raises builtin `TimeoutError`; asyncio cancellation remains
`CancelledError`. Native authentication, transport, protocol, overload and state
failures raise `rusty_haystack.ClientError` with sanitized diagnostics. Local
argument/conversion exceptions keep their Python types.

Reads accept a positive limit of at most 1000 (default 100), up to 8192 UTF-8 bytes
of filter text, and at most 1 MiB of response bytes before decoding. Declared and
streamed response sizes are checked. Responses exceeding the requested row limit
are rejected, never truncated. At most 16 native operations, including their
result-conversion guards, are admitted at once. Overload is rejected immediately;
there is no unbounded pending-operation queue. This small API has no arbitrary raw
operation or complete H5 operation family. Bounded remote watches use the separate
owned surface below and share this client's admission/runtime/session.

`state` reports `new`, `connecting`, `open`, `closing` or `closed`; `closed` is true
only after actual teardown. Close seals admission immediately, cancels requests,
destroys the owned native runtime and joins its thread, then drains operation and
result-conversion guards. This includes already-started DNS/SCRAM blocking work
that cannot be preempted by cancelling its request. Repeated close observes the
same cleanup. A cancelled or timed-out close waiter does not stop cleanup and is
not a graceful-close receipt. A valid expired close deadline still starts cleanup
and raises `TimeoutError` unless the owner was already closed. Invalid deadlines
raise before starting cleanup. If context-body failure and cleanup failure both
occur, the original body exception propagates, with a cleanup note when possible.

Dropping the Python owner requests cleanup without blocking or making Python
callbacks; it does not prove graceful completion. The process-wide asyncio bridge
and legacy synchronous runtimes have separate lifetimes. Read conversion and result
notification reserve Python attachment admission before waiting for the GIL.
A private normal-exit hook seals admission and releases the GIL while admitted
callbacks finish; later jobs cannot attach. Notifications whose owning event loop
has already closed are discarded. This does not join every native operation or
shut down the shared runtime. Abrupt/fatal exit, an unregistered exit hook,
subinterpreters and arbitrary callbacks that block forever are outside this
normal-exit boundary. Selected host subprocess tests cover unfinished work, but
only awaited successful close is the native teardown receipt. The synchronous
`HaystackClient` and `WsClient` remain blocking APIs with their existing ownership
behavior.

TLS uses verified system trust plus optional custom trust and identity. Pass
`tls=TlsConfig.with_ca(ca_pem_bytes)` for an additional CA, or
`TlsConfig.from_files(cert_path, key_path, ca_path)` for mutual TLS. The explicit
`tls_verify=False` lab/development option disables certificate-chain and hostname
verification while retaining any supplied CA/client identity. Redirects and
automatic retries remain disabled. HTTP authentication and reads reuse the same
TLS configuration; the WebSocket watch transport has its separate trust policy.

The selected runtime evidence is CPython 3.12 on macOS arm64; installed stub checks
target Python 3.11. Broader platform and free-threaded qualification is separate.
The independent TLS tests require an OpenSSL CLI and generate temporary seven-day
certificates so platform certificate policies and stale fixtures are respected.

## Owned synchronous HTTP client

`SyncHaystackClient` provides direct blocking H4 `connect`, `read`, and `close`
operations with the same native HTTP/SCRAM owner as `AsyncHaystackClient`.
Constructing it performs no I/O. Context entry connects and returns the client;
context exit closes it and preserves an existing body exception, adding a
cleanup note when possible.

```python
from rusty_haystack import SyncHaystackClient

with SyncHaystackClient(url, username, password, timeout=30.0) as client:
    sites = client.read("site", limit=100, timeout=5.0)
    print(len(sites))
```

`connect()` and `close()` return `None`; `read()` returns an `HGrid`. `state` and
`closed` are observations available from any thread. The first valid blocking
operation binds the client to one OS thread, including when that operation is
`close()`. Later blocking calls must use that same thread. Calls from a running
asyncio loop or an entered Tokio runtime raise `RuntimeError` before admission or
state changes. A complete construct/connect/read/close flow can run inside one
`asyncio.to_thread` call. Separate `to_thread` calls need not reuse a thread.

The constructor budget defaults to 30 seconds. Constructor and per-call `timeout`
values must be finite, positive, and representable at native timer resolution.
Per-call values only shorten the constructor budget. Each call converts its
relative budget once; native waiting, reacquiring Python, and result conversion
all consume it. Invalid inputs fail before thread binding or dispatch. A read
timeout cancels that request and permits later reads; a connect failure or timeout
is terminal. `TimeoutError` reports expiry, and `ClientError` reports native
failures. Reads default to 100 rows, allow at most 1000 rows and 8192 UTF-8 filter
bytes, and reject responses larger than 1 MiB. TLS configuration uses the same
verified defaults, custom CA/client identity, and explicit `tls_verify=False`
lab option described above.

Native waits release Python so other Python threads can run. Signal delivery,
including `KeyboardInterrupt`, may wait until the bounded native wait returns;
this facade does not promise prompt signal polling. A tiny valid close budget
still requests cleanup. If close times out, cleanup continues and a later close
can observe it. Successful close waits for the owned native thread, its runtime
and admitted result conversions to finish. Process-wide bridge threads remain
available to other clients. Dropping a client requests cleanup without waiting;
use successful explicit close for a teardown receipt. Close does not establish
remote SCRAM-session revocation. Supervised pending-work exit tests qualify the
selected host only; process exit is not proof of pre-exit native teardown.

This facade is exported from both `rusty_haystack` and `rusty_haystack.client`.
It has no H5, watch, or generic raw-call methods. Existing `HaystackClient` and
`WsClient` APIs retain their wider capabilities and existing lifecycle behavior.

## Selected H5 async reads

Pass `h5=H5Config()` to select the pinned preview for `read_by_id` and
`read_by_ids`. `H5Config` currently accepts only `version="5"`, `format="jeto"`,
and `boxing="all"`; other selections raise `ValueError` before dispatch. This
configuration applies to these two methods. The existing `read` and watch APIs
keep their existing protocol. There is no automatic negotiation or fallback.

```python
from rusty_haystack import AsyncHaystackClient, H5Config, H5Error, Ref

async def read_entities(url: str, username: str, password: str):
    async with AsyncHaystackClient(url, username, password, h5=H5Config()) as client:
        entity = await client.read_by_id(Ref("site-1"))  # HDict
        missing = await client.read_by_id(Ref("missing"), checked=False)  # None
        rows = await client.read_by_ids(
            [Ref("site-1"), Ref("missing"), Ref("site-1")], checked=False
        )  # HGrid: three rows, with an empty middle row
        try:
            await client.read_by_id()  # nullable id defaults to absence
        except H5Error as error:
            assert error.spec == "sys.api::UnknownEntityErr"
            assert error.status == 404
        return entity, missing, rows
```

Arguments follow the retained `sys.api::readById` and `sys.api::readByIds`
signatures at Project-Haystack/xeto revision
`873b922451d3ef4c0c9c08ef3daa542f352d69f3`. A lookup takes a `Ref`; its display
text does not change lookup identity. Omitted/`None` single IDs mean absence.
`checked=True` is the default and a missing entity fails the whole request.
Unchecked batch results keep input order, duplicates, and one empty `HDict` row
for each missing or inaccessible entity. Empty input yields an empty grid.

Requests use qualified function paths, `Xeto-Version: 5`, and contextual Jeto
`application/json;box=all`. Responses must report version 5 and admitted Jeto
JSON; Hayson, Zinc, unknown type contexts, malformed or mismatched identities,
wrong cardinality, and unsupported content encodings are rejected. The native
codec context and argument/result fitting derive from the retained HTTP catalog;
remote project catalogs are not loaded. This is a selected H5 preview, not a
complete library or operation-family implementation.

Supported values retain the existing `Int`, `Float`, `Number` (including units),
`Buf`, `NoneValue`, temporal and nested container mappings. Python `None` denotes
Haystack Null and remains distinct from `NoneValue`. Jeto Dict null members
denote absent fields, including batch placeholders. Unsupported exact output,
such as an unrelated nominal type or a noncanonical NaN payload on the current
server, produces a typed error rather than lossy substitution. A Time finer than
Python microseconds raises `ValueError`; the request admission is still released.

`H5Error` subclasses `ClientError` and exposes the admitted error `spec`, the
authoritative HTTP `status`, and an owned `HDict` `payload`. The payload's status
is advisory and can differ from HTTP status. Its `dis` and other fields are
untrusted peer data, excluded from the exception message. Malformed or unadmitted
error envelopes raise a sanitized `ClientError` instead. Local signature errors
retain `TypeError`/`ValueError`, local deadlines raise `TimeoutError`, and Python
cancellation remains `CancelledError`.

The same 16-operation owner bound, original session/TLS configuration, loop
affinity, cancellation, and close guarantees apply. Each batch accepts at most
1000 IDs of at most 8192 UTF-8 bytes each; encoded requests and collected response
bodies are bounded to 1 MiB, with additional codec work/depth/allocation bounds.
Batch length and borrowed Ref byte lengths are validated before owned ID copies.
The original absolute deadline and cancellation authorities are checked at codec
charges and around catalog initialization, fitting, identity checks, dispatch,
and result conversion. They do not reset between encoding and decoding. A caller
may observe completion later when its Python event loop resumes; the deadline
does not bound event-loop scheduling latency.
Tests use an independent wire peer and the first-party managed HTTP application.
Installed CPython 3.12/macOS arm64 evidence and Python 3.11 stub checks do not
qualify upstream-server interoperability, other platforms, free-threaded Python,
package publication, or navigation/history/watch/point-write H5 functions.

## Bounded remote async watches

A connected `AsyncHaystackClient` has at most 32 creation-key reservations,
counting both live children and locally closed children whose remote cleanup is
unconfirmed. `watch()` returns a handle synchronously, before any watch I/O, and
reserves its key to one logical owner in that parent's session. Duplicate keys
raise `ClientError` before registration or dispatch, including after ACK or a
locally joined Unknown cleanup. Dropping that handle or waiting out its lease
locally does not release the reservation. Capacity is bounded and unconfirmed
keys are never evicted to admit more work.

A reservation is reusable after no Create was dispatched or after confirmed
remote close/terminal release and local join. Parent session disposal clears its
remaining reservations. The server may still retain a closed-key tombstone, so
reusing a released local key can return `closed`; use a new key for a fresh
observation. Every handle retains the exact caller-supplied ID order/duplicates
and finite lease. The server
must advertise the first-party `state-subscription-v1` extension and negotiate
`haystack.state-subscription.v1` for WebSocket requests. This is distinct from
legacy synchronous `WsClient` watches and from Haystack 5 transport support.
The controlled embedded Python application does not install a watch provider.

```python
from rusty_haystack import AsyncHaystackClient, WatchDelivery

async def receive_one(url: str, username: str, password: str):
    async with AsyncHaystackClient(url, username, password) as client:
        watch = client.watch(["zone-temp", "zone-setpoint"], key="zone-view",
                             lease_ms=60_000, cleanup_timeout=2.0)
        async with watch:
            async for event in watch:
                if isinstance(event, WatchDelivery):
                    # Initial delivery contains the complete authorized records;
                    # later deliveries contain complete changed records/removals.
                    result = (event.changed, event.removed)
                    await watch.ack(event)
                    return result
                raise RuntimeError(f"watch control: {event.kind}/{event.reason}")
```

Use `async with` or await `watch.aclose()` in `finally`: ordinary `async for`
`break` does not close a custom iterator. Entry is a **Coroutine** adapter that
retains cleanup ownership through Task handoff; `ready`, `next`, `__anext__`,
`ack`, `resume`, `renew` and `aclose` return asyncio **Futures**. For a TaskGroup
or `create_task`, wrap those Future-returning methods in an `async def`, as for
client reads. `__aiter__` returns the same handle. Directly awaiting the handle
is not supported. Each child inherits its parent's exact loop/thread affinity
and caller context. Deadlines use the same absolute loop clock and constructor
budget as client reads. A deadline reached before dispatch raises `TimeoutError`;
a lost/timed-out dispatched operation can instead return `kind="unknown"`,
because the remote result may already have committed.

There is one native pending delivery and at most one unfinished operation per
child, including Python result conversion. Overlapping operations and
next-before-ACK fail locally without dispatch. There is no background polling
queue; `next()` polls only while the caller awaits it. `pending` exposes the
retained snapshot. A delivery's authority/watch/key, dataset/incarnation,
catalog/scope generations, token, integer `from_revision`/`through`, `initial`
and tuple containers are read-only. Each access to `changed` returns fresh,
mutable `HDict` copies, including independent nested values; caller edits cannot
change the retained native snapshot or ACK intent. `removed` contains ID strings.
Updates represent coalesced current state, not every intermediate write. Treat
changed rows as full replacements so deleted tags do not survive in local state.

ACK is always explicit: `await watch.ack(delivery)` must identify this child's
exact pending delivery. Yield, cancellation, early exit and mutable record edits
do not acknowledge anything. After a lost ACK response, `pending` and the exact
ACK intent remain retained. `await watch.resume()` reconciles that same ACK,
or otherwise resumes from the last acknowledged position. It may replay the
pending delivery. Recovery reuses the original authenticated session; logging in
again with the same username is a new session. An uncertain handle rejects
ordinary `ready`/`next` until explicit recovery. `await watch.renew(lease_ms)` is
the only renewal action. There is no automatic renewal, reconnect or resubscribe.

`WatchControl.kind` distinguishes `ready`, `authority`, `idle`, `acknowledged`,
`renewed`, `closed`, `rejected`, `resync`, `unknown`, `not_created` and `stopped`.
Rejected reasons preserve `invalid`, `forbidden`, `conflict`, `capacity`, `limit`
and `pending`. Resync preserves `authority`, `session_expired`, `revoked`,
`lease_expired`, `gap`, `incarnation`, `catalog`, `policy`, `scope_changed`,
`shutdown`, `overflow` and `unknown`. Gap, overflow, lease expiry or another
terminal resync is yielded once, then the old iterator ends. Start a fresh watch
with a new key explicitly when a new observation is appropriate. Unknown is not
an empty delivery or successful exhaustion. `state` reports `new`, `open`,
`uncertain`, `terminal`, `closing` or `closed`.

Create and cleanup use the parent's retained HTTP session. Poll/Resume/ACK/Renew
use a same-authority WebSocket with that same private bearer session. Failed WS
attachment after successful Create retains cleanup ownership. HTTPS uses the
client's configured HTTP trust; the current WSS attachment uses verified system
roots and does not inherit custom HTTP CA or mutual-TLS identity configuration.
There is no insecure verification fallback.

`aclose()` eagerly seals admission, even if its returned Future is immediately
cancelled or its valid observation deadline has expired. Repeated calls observe
the same supervised cleanup. One cleanup budget (default 2 seconds, positive and
at most 30 seconds) and a maximum of six requests cover all reconciliation and
Unsubscribe attempts. Cleanup can spend at most five requests on identity
reconciliation: the final request is reserved for Unsubscribe. It never sends a
possibly allocating Create with only one request slot left. An already-dispatched finite Create remains supervised.
If its response is missing, cleanup may retry **the exact original Create under
the same key/session** only to obtain its identity and immediately Unsubscribe.
If the first request never committed, this narrow cleanup step may perform that
original creation for the first time. Both a correlated Delivery and a legal
Create Idle retain the returned identity for immediate Unsubscribe. Idle is not
remote-release proof. Startup can accept such an Idle and continue from its
scope/acknowledged position without inventing a delivery. Cleanup never changes
key/IDs/lease, starts a replacement login, sends ACK/Renew, or logs out the parent
to close one child.

The close report's `local_joined=True` and `watch.closed` mean the child's native
work, socket and conversion ownership have drained. A `closed` response confirms
remote Unsubscribe; `not_created` means no Create was dispatched. An Unknown or
rejected cleanup, or resync with unknown authority/identity, remains unconfirmed;
the finite server lease is a separate bound. These reports retain the key
reservation. Context exit raises `ClientError` for any result without explicit
release proof, including Idle; it cannot make an unconfirmed result successful.
If the body already raised, its exception is preserved with a cleanup note when
possible; a repeated `aclose()` still exposes the report.
Other resync reasons retain the server's explicit terminal reason. Closing a
child preserves sibling watches and parent reads. Parent close drains all
children before attempting session logout and destroying its runtime; a failed
logout does not turn an unknown child report into confirmed release. The server
reclaims inactive-session bindings on its maintenance tick after logout.

Normal-exit and closed-loop tests cover selected CPython 3.12/macOS cases with
unawaited Create/receive work and with explicit close. Process exit or socket EOF
alone does not prove remote Unsubscribe or graceful native join. The interpreter
shutdown and platform limitations in the async-client section still apply.

## Controlled embedded applications

`ApplicationBuilder` starts an in-memory application using the same native
application lifecycle and bounded read service as Rust embedding. `start()` is
synchronous, requires a running asyncio loop, consumes the builder once, and
returns the sole `ApplicationOwner` immediately. Configuration is fixed at that
point. Await `ready()` for the actual `(host, port)` after initialization, or use
the owner as an async context manager:

```python
import asyncio
from rusty_haystack import ApplicationBuilder, HDict, Marker, Ref, SharedGraph

async def main():
    graph = SharedGraph()
    graph.add(HDict({"id": Ref("a"), "site": Marker()}))
    app = ApplicationBuilder(graph, port=0).start()
    async with app:
        address = await app.ready()
        view = app.handle()
        page = await view.read("site", limit=100)
        print(address, page.meta()["complete"], len(page))
    assert app.closed
    assert (await view.terminated()).close_error is None
    assert graph.contains("a")  # the caller's graph remains usable

asyncio.run(main())
```

The default listener binds `127.0.0.1` and ephemeral port `0`. `host` accepts
numeric IPv4/IPv6 addresses, avoiding separate hostname-resolution work. An
omitted graph creates an empty in-memory graph. A supplied `SharedGraph` remains
shared mutable state owned by its caller; configuration freeze does not freeze
entity contents. Closing an application releases its listener and admitted work,
and borrows the process-wide bridge runtime throughout. Other applications and
clients continue using that bridge after closure.

Direct Python reads use explicit trusted-embedding authority. HTTP entity and
catalog reads require an authenticated principal with read/admin permission.
Pass a fully configured `rusty_haystack.server.AuthManager` as `auth=` to enable
SCRAM. The manager is consumed only when `start()` validates the loop and claims
the configuration; invalid construction or a start without a running loop leaves
it usable. With `auth=None`, public HTTP protocol metadata remains available,
but entity data is denied. Remote requests cannot obtain trusted-embedding
authority by supplying a subject or header.

This profile selects scoped reads and the existing native read/catalog/system
HTTP routes. It selects no mutation, history, subscription, watch, custom-router,
database or Python-callback providers. Capability discovery derives from that
native selection. The embedded/HTTP read evidence covers a shared in-memory read
service and lifecycle; it does not establish parity with every legacy CLI
capability/default or qualify PostgreSQL. Legacy `HaystackServer.run()` and
`run_background()` retain their existing behavior and are outside this profile.

`ApplicationHandle` is a borrowed readiness/read/status/termination view with no
`close` method. Dropping it has no shutdown effect. Keeping it does not keep the
owner alive; owner closure makes its retained handles terminal to new reads.
`read(filter, limit=100, cursor=None, deadline=None)` returns an `HGrid` with native
`complete` and optional `cursor` metadata. Pass the cursor with the same query and
page size to continue. Native cursors remain scoped to the service and principal;
this API does not promise cross-principal HTTP/Python cursor exchange.

Reads allow 1–1000 rows, filter/cursor strings up to 8192 UTF-8 bytes each, and
the native conservative 1 MiB output budget. The shared service retains its other
finite defaults, including eight executing workers, sixteen queue slots and a
five-second read budget. At most sixteen Python read/conversion observations are
pending per application across all its views. Native workers remain tracked if a
Python observer is cancelled; a parent work guard also covers Python conversion,
including a conversion error, before termination can complete.

The owner and its handles bind to the exact loop and OS thread that called
`start()`. `ready`, `read`, `close`, `terminated` and `__aexit__` return Futures
and support the same task patterns as `AsyncHaystackClient` above. `__aenter__`
returns a coroutine-compatible awaitable so cancellation is handled in the task
performing context entry. Its readiness observation begins when awaited. Await
it directly through `async with`; a separately scheduled entry task
has its own result handoff and requires its caller to retain shutdown ownership.
An optional absolute `deadline` uses that loop's
`time()` clock, is converted once, and can shorten the thirty-second observation
budget; a read additionally retains the native five-second budget. Expired reads
are rejected before admission. Invalid/nonfinite deadlines fail before mutation.
Ordinary cancelled readiness observers leave the application running. Failed or
cancelled `__aenter__` requests shutdown, including cancellation after native
readiness but before its awaiting task resumes. Disposing of an unstarted entry
coroutine or cancelling it before its first poll also requests shutdown. Retain
the owner before awaiting so cleanup can always be observed through `close()`.

`close()` seals native admission before returning its Future. Cleanup continues
independently of cancelled/expired waiters, and repeated close observes the same
result. A valid already-expired close still seals and raises `TimeoutError` unless
termination was already complete. Successful close waits for actual native
termination: owned accepted sockets, workers, conversion guards and resource
cleanup have finished. The native policy permits one second of drain followed by
four seconds of cooperative stop; exceeding that allowance records a failure and
retains cleanup until it actually finishes. An observation deadline can return
before that happens without claiming completion.

`state` preserves native `starting`, `running`, `closing`, `closed` and `failed`.
`failed` alone is not a cleanup receipt. `closed` means termination has actually
completed, including a failed outcome. `terminated()` returns an immutable
`ApplicationTermination` with `close_error`, `cleanup_errors` and `drain_expired`
(the latter is `None` when native close failed). An earlier native timeout remains
in this report even after cleanup succeeds; repeated close raises the retained
`ApplicationError`. Observation/read deadlines raise built-in `TimeoutError`,
asyncio cancellation remains `CancelledError`, and local argument/conversion
errors keep their Python types. Context exit preserves an original body exception
if cleanup or cleanup-note annotation also fails.

Owner destruction requests native cleanup without blocking or Python callbacks;
await close/termination for evidence. The shared bridge's normal-exit admission
and callback-drain boundary, with the limits described above, also applies here.
Supervised unfinished exits
and installed consumers qualify the selected macOS arm64/CPython 3.12 environment;
Python 3.11 is the static typing floor, not an additional runtime qualification.
