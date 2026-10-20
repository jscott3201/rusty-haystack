# Rusty Haystack

Rust libraries, a CLI and Python bindings for [Project Haystack](https://project-haystack.org) building data: typed values, entity graphs, filters and codecs, managed applications, HTTP clients and servers, optional PostgreSQL and SQLite storage, and read-only MCP tools.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

This README describes the **0.9.0 source on `dev`**. Build this checkout to use the capabilities below; a previously published crate, wheel or executable may have a different API. Haystack 4 interfaces and a selected Xeto/typed HTTP profile are available. The selected typed profile does not claim complete Haystack 5 or upstream runtime interoperability.

## What you can build

- **Model and query building data.** Work with Dicts, Grids, references, units, Haystack filters, indexed graphs, relationship traversal and the bundled ontology. Exact typed Int, Float, None, Buf and nominal values have explicit conversion and encoding rules.
- **Exchange and validate data.** Read and write Zinc, Haystack JSON and Trio; export CSV. Contextual Jeto and the project-owned typed payload preserve selected richer values. Load Xeto libraries, inspect and fit admitted specs, and validate entities against the selected catalog.
- **Embed a bounded application.** Share authorization, catalog observations, paging, cancellation and resource limits across Rust reads and scoped HTTP adapters. Select entity mutations, history reads/writes, state subscriptions and local point-write simulation when your application needs them.
- **Use typed contracts and generated clients.** Admitted Xeto signatures drive argument defaults, fitting, discovery and encoding. Scoped applications export OpenAPI 3.1 and JSON Schema 2020-12; an independent generated-client fixture exercises the selected contract over real HTTP.
- **Choose storage and access profiles.** Use process-memory services or explicitly select PostgreSQL durability or a local SQLite entity and history authority. Python offers semantic values, graph APIs, an async HTTP client and a controlled embedded application. MCP exposes two authorized read tools over stdio or protected local HTTP.
- **Consume remote history incrementally in Rust.** The Zinc `his_read_stream` session preserves remote observations, delivers bounded batches and validates completion at EOF. [History client contract](docs/history-reads.md#incremental-rust-http-consumption).

![An admitted catalog supplies the executable registry. Scoped application requests share policy, budgets and lifecycle ownership across Rust and HTTP adapters and schema export; optional service owners are selected by the application.](docs/images/admitted-api.svg)

## Start from source

Use the repository-pinned Rust **1.99.0** toolchain:

```sh
git clone --branch dev https://github.com/jscott3201/rusty-haystack.git
cd rusty-haystack
cargo +1.99.0 build --locked --workspace --exclude rusty-haystack
cargo +1.99.0 run --locked -p rusty-haystack-cli -- serve --demo --port 8080
```

The excluded `rusty-haystack` package is the PyO3 extension; build it through Maturin with a Python interpreter as shown below. The demo uses process memory and the legacy HTTP profile, listening on `127.0.0.1:8080`. In another terminal:

```sh
curl http://127.0.0.1:8080/api/about
curl -X POST http://127.0.0.1:8080/api/read \
  -H 'Content-Type: text/zinc' \
  -d 'ver:"3.0"
filter
"site"'
```

To install a standalone CLI from the checkout:

```sh
cargo +1.99.0 install --locked --path haystack-cli --root "$PWD/haystack-install"
./haystack-install/bin/haystack --help
```

Select optional CLI features in that install command according to the profile you need:

| Profile | Selection | Entry point and contract |
| --- | --- | --- |
| Core Rust data model | `rusty-haystack-core`; optional `chrono-tz` feature | Trusted in-process graph, values, filters and codecs. [Typed values](docs/typed-values.md) |
| Managed Rust application | `rusty-haystack-app` and chosen adapters/services | One application owner, borrowed handles and Tokio runtime. [Shared reads](docs/shared-reads.md), [lifecycle](docs/application-lifecycle.md) |
| Ordinary CLI/HTTP demo | No optional feature required | `haystack serve --demo`, data tools and HTTP client commands. [CLI](docs/cli.md) |
| SQLite Rust application | `rusty-haystack-app` with `features = ["sqlite"]` | One local-file owner, durable entity mutations/feed and bounded history upserts/receipts; no CLI selector. [SQLite contract](docs/sqlite-durability.md) |
| PostgreSQL CLI | `--features postgres` | `haystack serve --postgres-config FILE --users FILE`; explicit database and SCRAM configuration. [Standalone workflow](docs/standalone-postgres.md) |
| MCP stdio CLI | `--features mcp` | `haystack mcp stdio --config FILE`; trusted launch scope and Unix pipes. [Stdio configuration](docs/mcp-stdio.md) |
| MCP HTTP CLI | `--features mcp-http` | `haystack mcp http --config FILE`; numeric loopback, issuer/public keys and per-subject scope. [HTTP configuration](docs/mcp-http.md) |
| Python | Maturin source build below; Python ≥3.11 package requirement | Values, graphs, async client and controlled in-memory application. [Python API](rusty-haystack/README.md) |

`--features mcp,mcp-http` installs both independent MCP commands. The MCP library selects `stdio` and `http` separately. These options do not make PostgreSQL the MCP CLI dataset provider or enable every application service. Default MCP and application crate features are empty; optional profiles need their own configuration and qualification.

## Managed applications and typed HTTP

`EntityGraph` is a trusted in-process data structure. `ApplicationBuilder` adds an explicit read policy, finite budgets and lifecycle ownership. Its handles give adapters the same managed service, so authorization, hidden tags, reference disclosure, current catalog observations and continuation checks apply at the shared boundary. New pages recheck policy; a cursor is not an authorization grant.

The builder fixes configuration at start and produces one `ApplicationOwner`. Readiness follows initialization of the selected providers and listeners. Handles and the Tokio runtime are borrowed. Await `close()` for the shutdown result and `terminated()` to observe actual cleanup after an unsuccessful or cancelled close observation; an embedding that owns the runtime drops it outside async after termination. Dropping an owner requests cleanup but is not a completed cleanup receipt. See [Application lifecycle](docs/application-lifecycle.md) for seal/drain behavior and resource ownership.

Entity mutation, history, subscription and command services are explicit selections. A schema declaration alone installs no handler or provider. Mutation APIs preserve committed, rejected and unknown outcomes; transport success alone does not establish an effect, and an unknown outcome does not authorize blind replay. Process-memory providers do not survive restart. Point-write support is a **local simulator**, with no physical equipment command authority.

Trusted catalog activation validates affected data and publishes an immutable observation. A typed invocation retains one observation for resolution, argument/default handling, fitting and result encoding. The executable catalog is a bounded admitted subset; see [Typed HTTP](docs/typed-http-read.md), [Jeto](docs/jeto.md) and [repository serving admission](docs/repository-serving-admission.md).

A **separately configured managed scoped server** exposes this project-owned schema endpoint; the CLI demo above does not select it:

```sh
curl -H 'Xeto-Version: 5' -H 'Accept: application/json' \
  http://127.0.0.1:8080/api/rusty-haystack/openapi.json
```

Supply that server's authentication header when enabled. Its caller-filtered export describes qualified POST operations with `Xeto-Version: 5` and `application/json;box=none` output. `x-rusty-haystack-json-schema` contains the standalone Draft 2020-12 library. Unsupported visible declarations have explicit exclusions; hidden declarations remain undisclosed. JSON Schema validates JSON shape, but cannot prove nominal identity, resolve graph references or preserve every rich value/ambiguous union without boxes. [Schema export and generated clients](docs/schema-export.md) documents the executable subset and independent validation recipe.

## Python from this checkout

The package requires Python **3.11 or newer**. The source-build and test recipe uses CPython **3.12** and pinned Maturin **1.15.0**:

```sh
uv venv --python 3.12
uv pip install maturin==1.15.0 pytest
source .venv/bin/activate
maturin develop --locked --release -m rusty-haystack/Cargo.toml
pytest rusty-haystack/tests -q
```

The bindings include core values, filters, graphs, codecs and static typing. Use `Int`, `Float`, `NoneValue`, `Buf` and `Nominal` when their typed identity matters; native Python integers mapped to Haystack Number must be exactly representable within the documented limits. Rich typed payloads and H4 codec projection have separate contracts. See [Python values and codecs](rusty-haystack/README.md#codecs) and [typed values](docs/typed-values.md).

A controlled embedded application shares a bounded native read service with its selected HTTP listener:

```python
import asyncio
from rusty_haystack import ApplicationBuilder, HDict, Marker, Ref, SharedGraph

async def main():
    graph = SharedGraph()
    graph.add(HDict({"id": Ref("site-a"), "site": Marker()}))
    owner = ApplicationBuilder(graph, port=0).start()
    async with owner:
        print("Listening:", await owner.ready())
        page = await owner.handle().read("site", limit=100)
        print("Rows:", len(page), "Complete:", page.meta()["complete"])
    assert graph.contains("site-a")

asyncio.run(main())
```

`start()` runs inside an asyncio loop and returns the owner synchronously; `async with` awaits readiness and owns shutdown. Handles have no close authority, and the supplied graph remains usable after close. This profile selects in-memory reads, catalog and system routes. Direct Python reads use trusted embedding authority; HTTP entity data requires explicitly configured authentication. It does not install database, mutation, history, watch or callback providers.

For remote HTTP, [`AsyncHaystackClient`](rusty-haystack/README.md#async-http-client) owns its HTTP/SCRAM session and native I/O resources. It binds to one exact loop/thread, supports deadlines and request cancellation, and joins owned resources on successful close. Its [bounded remote watches](rusty-haystack/README.md#bounded-remote-async-watches) retain one pending typed delivery with explicit ACK, renewal and recovery, and close independently of siblings. Its async operations return asyncio Futures: await them directly or wrap them in a coroutine for `create_task`. The [controlled application contract](rusty-haystack/README.md#controlled-embedded-applications) explains owner/handle shutdown, cancellation and interpreter-finalization limits. Python package metadata and release matrices do not establish runtime support on every listed interpreter or platform; consult the [support matrix](docs/support-matrix.md).

For synchronous scripts, [`SyncHaystackClient`](rusty-haystack/README.md#owned-synchronous-http-client) provides owned H4 connect/read/close with direct return values, one OS thread per client, and finite relative timeout budgets. Native waits release Python, and successful close joins owned native resources. Existing `HaystackClient` and `WsClient` capabilities remain available.

## Optional SQLite storage

The application crate's `sqlite` feature selects a single-owner local file using bundled SQLite **3.53.2**. Entity CRUD, expected revisions, retained receipts and change-feed cursors survive close/reopen. Bounded live history reads and authorized one-point timestamp upserts retain samples, generations and receipts in a separate history authority. The [SQLite contract](docs/sqlite-durability.md) describes format 2, native cleanup, process-crash qualification and scope; a CLI selector and SQLite Python wheels are not selected.

## Optional PostgreSQL storage

The `postgres` application/CLI feature selects bounded durable entity and history services for cooperating processes sharing one database/schema. It supports verified TLS, retained operation receipts, coherent admission refresh, bounded periodic cache convergence, restart recovery and bounded entity snapshot sessions. Runtime qualification uses PostgreSQL **18.6**. History remains bounded live data; entity and history mutation domains do not form one atomic combined commit.

The [standalone PostgreSQL workflow](docs/standalone-postgres.md) covers installation, explicit private configuration, SCRAM users, useful writes/reads and recovery after restart. Its disposable recipe installs a fresh CLI and provisions its own test database:

```sh
python3 scripts/postgres/standalone.py --docker
```

Follow that document's prerequisites before running the recipe. [PostgreSQL durability](docs/postgres-durability.md) defines the selected storage guarantees. [Backup and restore](docs/postgres-recovery.md) provides bounded authority transfer between distinct schemas in the same surviving database/control journal; it does not establish cross-database disaster recovery or production deployment qualification.

## Read-only MCP tools

Both MCP transports expose only `haystack_query` and `haystack_read`, with fixed schemas and bounded H4 JSON results by default. An explicit launch may select the [rich Jeto profile](docs/mcp-rich-values.md) for exact typed scalars, admitted nominal provenance and Buf values. They share application read policy, hidden-tag/reference filtering and principal-bound continuation checks. No write, history, command, resource, prompt or task capability is exposed.

[Stdio](docs/mcp-stdio.md) uses an explicit trusted launch principal and dataset scope. A host launches the installed command with cancellable **Unix pipes on both stdin and stdout**; an interactive terminal or regular-file redirection is not the selected runtime profile. It supports the documented modern discovery and legacy initialization revisions.

[Streamable HTTP](docs/mcp-http.md) uses modern MCP `2026-07-28`, numeric loopback HTTP/1 and one POST per connection with a JSON response. Configure the issuer, trusted RS256 public keys, required read scope and admitted subjects before launch. Every request verifies its signed access token and current local revocation set. Resource metadata and challenges identify the actual bound audience. Offline verification does not learn remote issuer revocations automatically.

```sh
# Each command requires its selected feature and a complete configuration.
./haystack-install/bin/haystack mcp stdio --config /absolute/path/mcp-stdio.toml
./haystack-install/bin/haystack mcp http --config /absolute/path/mcp-http.toml
```

The adapters bound input, response size, concurrent work and complete request deadlines. Shutdown retains work until native reads and completed/suppressed output are joined. Installed fixtures exercise independent schemas, scoped reads and real process/pipe/socket cleanup. This local HTTP profile does not claim SSE sessions, public TLS/reverse-proxy deployment, production identity-provider integration or external agent-host/model-call qualification.

## Workspace and documentation

| Crate | Role |
| --- | --- |
| [haystack-core](haystack-core/) | Values, codecs, filters, units, graph, ontology, Xeto admission and fitting |
| [haystack-app](haystack-app/) | Scoped reads, selected mutations/history/subscriptions/commands, catalog activation, optional PostgreSQL/SQLite and lifecycle ownership |
| [haystack-server](haystack-server/) | Axum HTTP adapters, SCRAM authentication and WebSocket transport |
| [haystack-client](haystack-client/) | Async Rust HTTP/WebSocket client, SCRAM and TLS configuration |
| [haystack-cli](haystack-cli/) | `haystack` data/validation tools, server/client commands, user management, storage administration and selected MCP launchers |
| [haystack-mcp](haystack-mcp/) | Optional bounded read-only stdio and authenticated local HTTP adapters |
| [rusty-haystack](rusty-haystack/) | Python values, graph/codec APIs, async client and controlled embedded application |

- Architecture and setup: [Architecture](docs/architecture.md), [Getting started](docs/getting-started.md), [CLI](docs/cli.md), [Configuration](docs/configuration.md), [Client](docs/client.md), [Server API](docs/server-api.md).
- Application services: [Shared reads](docs/shared-reads.md), [Lifecycle](docs/application-lifecycle.md), [Entity mutations](docs/entity-mutations.md), [History reads](docs/history-reads.md), [History writes](docs/history-mutations.md), [State subscriptions](docs/state-subscriptions.md), [Point-write simulation](docs/typed-http-read.md#local-point-command-simulation-m2-pr12).
- Types and contracts: [Typed values](docs/typed-values.md), [Jeto](docs/jeto.md), [Typed HTTP](docs/typed-http-read.md), [Schema export](docs/schema-export.md), [Xeto repository downloads](docs/repository-client.md).
- Development and evidence: [Contributing](CONTRIBUTING.md), [Support and validation matrix](docs/support-matrix.md), [Artifact validation](docs/artifact-validation.md), [Roadmap](https://github.com/users/jscott3201/projects/3), [Issues](https://github.com/jscott3201/rusty-haystack/issues).

The local full gate builds and tests Rust/Python surfaces and runs the independent schema/client proof. Optional profiles, hosted jobs and release artifacts have separately recorded coverage; one local pass does not qualify another platform or a published binary. First-party Rust forbids unsafe code; dependency internals have their own policies. Authentication, authorization and resource limits remain explicit application/deployment choices.

[Benchmarks.md](Benchmarks.md) records correctness-checked graph/history workloads, commands and measurement provenance. Historical March 2026 results describe v0.8.0 on Rust 1.93.1 and do not establish current latency, capacity or a speedup. Planned additional storage engines, Arrow/Parquet/DataFusion integrations, broader MCP capabilities and wider Python/free-threaded qualification remain roadmap work.

## License

[MIT](LICENSE). Retained upstream Xeto source keeps its source license and provenance alongside the pinned profile.
