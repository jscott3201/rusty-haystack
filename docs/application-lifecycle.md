# Application lifecycle

`haystack-app` supplies `ApplicationBuilder`, `ApplicationOwner` and cloneable
`ApplicationHandle` independently of HTTP. Each builder creates one managed
read service with the supplied graph, policy and limits. Its handle is available
before startup so every adapter receives exactly the same service and cursor
authority. The early handle returns `NotReady` until readiness. A new builder
creates a new authority; service clones share the existing one.

## Readiness and ownership

Consume configuration with `builder.start(&runtime_handle)`, then await
`owner.ready()`. The application borrows that Tokio runtime and never shuts it
down. A listenerless application is supported. Owned resources initialize in
registration order. Readiness follows all selected provider initialization and
all selected listener binds; its `ReadyInfo` contains actual bound addresses.
An HTTP listener binds before readiness but waits for overall readiness before
serving requests.

The states are `Starting`, `Running`, `Closing`, `Closed` and `Failed`. Only
`Running` admits new work. A failed initialization, owned background-task
failure, cleanup error or close timeout produces `Failed`; later cleanup does
not erase that outcome. Closing during startup cancels the initialization
attempt and rolls back partial acquisitions. Dropping an unstarted builder
marks its early handles failed without initializing any resources.

`ApplicationOwner` owns shutdown. `ApplicationHandle` and `ReadService` clones
keep access to the same state but do not independently own shutdown. Dropping
the owner atomically seals admission and requests nonblocking cleanup. Drop
cannot await completion: retain the owner and explicitly await close when a
completion receipt is needed. A handle can also await `terminated()` after the
owner has been dropped. Retained managed read handles return `Closed` after
sealing; dropping one handle or service clone does not stop other users.

`ReadService::new` remains an unmanaged compatibility API: it borrows the
caller's runtime, has request-level cancellation, and has no application-wide
close. Managed adapters accept an `ApplicationHandle` instead of replacing an
arbitrary service's policy or cursor identity.

## Owned resources and partial startup

Register resources with `ApplicationBuilder::owned_resource`. Implement
`ApplicationResource::initialize`, `rollback_start` and `close`. Initialization
must be cancellation safe: record each acquisition in resource state or an RAII
guard so dropping the initialization future leaves a cleanup path. A failed,
panicking or cancelled attempt receives `rollback_start`; successfully
initialized resources receive `close` in reverse initialization order after
owned work ends. Each hook is invoked once. A cleanup error is retained in the
termination report and is not evidence that an external resource was released.

`ResourceContext::spawn` registers background resource work before readiness.
The owner tracks actual future destruction, including panic or abort. Errors
and panics initiate application shutdown. Such tasks must respond to the
application's closing or cancellation token; a non-cooperative task can outlive
the public close deadline. Arbitrary tasks spawned directly onto a caller's
runtime remain that caller's responsibility.

`HaystackServer::into_listener()` transfers a selected HTTP listener to this
resource protocol. `with_scoped_reads(handle)` selects scoped reads;
`with_application(handle)` attaches a lifecycle while preserving the selected
profile (legacy by default). In either configuration order, an attachment from
another application is rejected before router publication. Only
`with_legacy_unrestricted()` explicitly changes a scoped profile to legacy.
The listener also rejects a handle from another owning builder or a graph with
different storage. The selected policy, cursor state and catalog graph remain shared.
For legacy standalone setup, `HaystackServer::start()` constructs a convenience
owner. `run()` and `run_reporting_addr()` are compatibility futures: dropping
one requests cleanup, while explicit owner APIs let callers await its completion.

History is selected once on `ApplicationBuilder` with `owned_history(service)`
or `borrowed_history(service)`, sharing its exact managed read authority. Owned
history initializes before listener resources, independently of builder call
order. A successful attempt receives `close` after actual work completion; a
failed or cancelled attempt receives `rollback_initialize`. Borrowed providers
receive no hooks. Provider resources must close independently of `Arc` lifetime.

Managed listeners and external routers use that exact application selection;
they reject an independently configured listener provider. Scoped history is
opt-in. Legacy standalone `HaystackServer::start()` translates its
`with_history_provider(Box)` or `with_borrowed_history_provider(Arc)` convenience
configuration into the same application ownership path. See
[bounded history reads](history-reads.md) for session and collector contracts.
`history_mutations(writes)` separately selects writes over that exact history
service. It adds no second provider lifecycle; opaque prepared writes retain the
original tracked work until publication, rejection or drop, and reject after
owner sealing. See [history mutations](history-mutations.md).

## Seal, drain, stop and completion

The first `owner.close()` call fixes the shutdown deadlines and seals new
admission atomically with request registration. Concurrent and repeated calls
share that same outcome. Registration happens before a body is collected or
work enters a queue. Already-admitted work can continue during the drain window;
its `WorkGuard::child` registration is a continuation held under the parent's
registration, not permission for unrelated new work.

The closing token stops owned listeners accepting connections. After the drain
allowance, the cancellation token asks admitted reads, bodies, legacy WebSocket
connections and their tracked writers to stop. Owned accepted TCP connections
also retain a registration through actual I/O destruction, beyond middleware's
response return. Stop wakes pending transport reads and writes, so a peer that
stops consuming a response cannot hold provider cleanup open. An immediately
writable final response or WebSocket close frame may finish; blocked transport
I/O is cancelled. Axum's connection completion wait remains in place.
Queued and running blocking read
workers retain their registration and capacity permit until actual task
completion or destruction, even after their caller has received a cancellation
error or dropped its future. Running blocking code cooperates between bounded
operations; shutdown does not interrupt arbitrary machine instructions.

Defaults allow one second to drain and four more seconds for stopping and
resource cleanup; `ShutdownPolicy` can configure both. Startup has a separate
30-second deadline. A close timeout returns `ApplicationError::ShutdownTimeout`
and `Failed`, including the observed task count. It does not claim that the
outstanding work joined. The coordinator keeps ownership and continues waiting
for actual work completion, then invokes resource cleanup. A cleanup hook that
exceeds the deadline also stays alive and observed instead of being cancelled.

A successful `close()` is a termination receipt. After an error, await
`terminated()` separately when releasing runtime ownership. `TerminationReport`
preserves the earlier close result and any cleanup errors; receiving it proves
that owned task observation and cleanup hooks ended, even if cleanup reported a
failure. There is no finite completion promise for non-cooperative code.

```rust,ignore
let owner = builder.start(&tokio::runtime::Handle::current())?;
let ready = owner.ready().await;
// Use the application only after ready succeeds.
let close_result = owner.close().await;
let termination = owner.terminated().await;
// Inspect readiness, close_result and termination.cleanup_errors separately.
```

Standalone callers that construct a `Runtime` keep it driving through
termination and drop it outside `block_on`. The CLI follows this sequence on
Ctrl-C and Unix SIGTERM. Embedded callers keep their borrowed runtime and
unrelated tasks usable after application shutdown. Python runtime/FFI lifetime
APIs are unchanged by this Rust owner boundary.

## Externally hosted routers

`into_external_router()` requires an application handle. History selection and
owned/borrowed provider lifecycle remain with that application, as with its
owned listeners; an independent adapter provider is rejected. The router gates
built-in requests and tracks managed reads and upgrades even when retained
after close. The caller owns its listener, HTTP connection tasks, response
transport and arbitrary custom-route tasks. Application termination does not
claim those caller-owned resources stopped. Custom-route authentication and
scoped-policy restrictions are documented in [shared reads](shared-reads.md).

`POST /api/close` is bearer-session logout. It does not request application
shutdown. Persistent-store recovery and publication authority, scoped watches, and Python lifecycle bridging are separate boundaries.
