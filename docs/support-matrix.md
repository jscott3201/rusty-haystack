# Support and validation matrix

This matrix describes the checks defined by the tracked workflows and local gate.
For a particular revision, qualification requires a successful run at that revision;
the existence of a workflow is not execution evidence. Dependency-lock presence does
not qualify an optional backend, interpreter, platform, or feature.

## Pull requests and branch pushes

`.github/workflows/ci.yml` runs on pushes and PRs for `dev` and `main`. PR events include
`edited`, so changing the base branch recomputes the matrix. The standard-library
policy in `scripts/ci/policy.py` rejects an unknown event, target branch, or profile.

| Surface | dev profile | main profile | Meaning of a successful check |
|---|---|---|---|
| MSRV Rust 1.99.0 workspace tests, excluding the PyO3 extension | Ubuntu | Ubuntu, macOS, Windows | Unit, integration and doctests on each selected host |
| Core `chrono-tz` tests | Ubuntu | Ubuntu, macOS, Windows | Separate optional-feature tests and doctests |
| Rustfmt and MSRV Clippy | Ubuntu | Ubuntu | Formatting; workspace all-target lint excluding PyO3; separate core `chrono-tz` lint |
| Core minimal and default | Ubuntu | Ubuntu | Package-only Clippy and tests, separately with and without `--no-default-features` |
| Current stable Rust 1.99.0 | Ubuntu | Ubuntu | Separate named lane with the workspace and `chrono-tz` Clippy/test selections |
| Python bindings | Ubuntu, CPython 3.12 | Ubuntu, CPython 3.12 | PyO3 Clippy, locked release-mode `maturin develop` with Maturin 1.15.0, then pytest |
| Schema and generated HTTP client | Ubuntu, Python 3.12 | Ubuntu, Python 3.12 | Locked OpenAPI/JSON Schema validators and unedited openapi-python-client output against a real local fixture; part of the existing Python job |
| MCP pipe profile | Ubuntu | Ubuntu, macOS adapter tests; Ubuntu installed fixture | Explicit `stdio`/CLI `mcp` feature tests and Ubuntu Clippy; fresh installed default H4 and opt-in Jeto discovery, independent boxed schemas/selected-value decoder, scope, EOF/signals and pipe backpressure; no Windows stdio claim |
| MCP local HTTP profile | Ubuntu | Ubuntu, macOS adapter tests; Ubuntu installed fixture | Explicit `http`/CLI `mcp-http` and combined feature tests; default H4 and opt-in Jeto, offline RS256 fixture issuer, SDK/raw HTTP, independent boxed schemas/selected-value decoder, caller scope, cancellation and socket backpressure; local JSON-response profile only, no production TLS/IdP or external host claim |
| SQLite 3.53.2 entity durability | Ubuntu x86_64 | Ubuntu x86_64 | Optional app SQLite and combined PostgreSQL/SQLite Clippy/tests, SQLite doctests, dependency isolation and native local-file process-crash/rollback/recovery/ownership qualification; history is not selected |
| PostgreSQL 18.6 durability | Ubuntu | Ubuntu | Optional app/CLI feature Clippy/tests, app doctests, real Docker verified-TLS durability qualification, and installed standalone auth/write/history/restart/signal cleanup |
| Cargo Deny | Ubuntu | Ubuntu | Locked all-feature dependency advisory, license, ban and source checks |
| CodeQL | Ubuntu, Rust/Python/Actions | Ubuntu, Rust/Python/Actions | Analysis and upload jobs completed using the existing query filters; the separate alert-results check is described below |
| CI policy | Ubuntu | Ubuntu | Process-level aggregate and matrix tests, workflow wiring regressions, benchmark capture input/receipt regressions, and artifact provenance/staging regressions |

The Rust floor and current-stable pins currently coincide at 1.99.0. Their names
express distinct compatibility contracts and do not imply coverage of two compiler
versions. Core currently has an empty default feature set; separate minimal and
default package invocations prevent workspace dependency feature unification from
standing in for either contract. Workspace builds can enable dependency features
that an isolated core build does not.

`CI OK` is the canonical workflow-job aggregate. Its explicit dependencies are `ci-policy`, `fmt`,
`clippy`, `test`, `current-stable`, `python`, `deny`, `postgres`, `sqlite`, `core-features`, and `codeql`.
It runs with `always()` and accepts only `success` for every required job. Missing
jobs, malformed or duplicate JSON keys, failure, cancellation, skip, unknown profiles,
or an unexpected planned OS inventory fail closed. Job results enter through an
environment variable, never interpolation into shell source. There are no optional
jobs in either profile. The general dev test matrix excludes macOS/Windows; the
PostgreSQL and SQLite selected profiles are explicitly Ubuntu-only on both target branches.

For CodeQL, `CI OK` confirms successful completion of the reusable analysis and upload
jobs. GitHub subsequently evaluates alert severity in the separate
`Code scanning results / CodeQL` check, which can fail after those jobs succeed.
Delivery must inspect that separate check when present; `CI OK` does not prove there
are no blocking alerts. See GitHub's documentation on
[code scanning results checks](https://docs.github.com/en/code-security/how-tos/manage-security-alerts/manage-code-scanning-alerts/triage-alerts-in-pull-requests).
CodeQL source analysis also makes no runtime or compilation qualification claim.

The policy tests independently assert the expected expanded OS lists, aggregate
dependency wiring, retarget trigger, and all three CodeQL languages. They detect a
removed `needs` edge or `always()` condition. A successful matrix-job result alone
cannot prove that the intended matrix entries were configured. Wiring assertions
deliberately guard the current workflow layout; they are not a general YAML parser.
Use `actionlint .github/workflows/ci.yml .github/workflows/codeql.yml` for independent
workflow syntax validation when editing those files.

The same CI policy job runs the nine benchmark capture-control tests in
`scripts/bench/test_capture.py`. They cover build-input admission, configuration
and toolchain provenance, and receipt completion/failure; they run no benchmarks
and do not establish timing results.

The CI policy job also runs the artifact provenance and staging tests in
`scripts/release/tests`. These exercise rejection paths and byte-preserving staging
with bounded fixtures. They do not build, install or execute a release artifact;
that evidence belongs to the separate artifact workflows.

CodeQL runs once through CI on pushes and PRs and retains standalone weekly/manual
execution. The separate Audit workflow checks both branches daily and on manual
dispatch. Scheduled/manual results are not results for a particular PR run. Branch
protection is external configuration; this matrix does not claim that `CI OK` is
configured as a required branch-protection check.

## Local gate

`./.agents/gate.sh --full` runs the policy, benchmark capture-control and artifact
provenance/staging tests,
formatting, the same Rust command
selections on the local host, core minimal/default checks, Python binding checks,
the locked schema/generated-client proof (uv and Python 3.12),
and Cargo Deny. Python binding checks require the repository `.venv` with CPython 3.12,
Maturin 1.15.0 and pytest; the extension is rebuilt before pytest. A missing `.venv`
is recorded as incomplete, while an existing but invalid environment is a failure.
The schema tooling uses `scripts/schema-client/uv.lock` and a separate environment;
missing uv is incomplete, while tooling or HTTP proof failures fail the gate.
This qualifies one generator and a synthetic loopback fixture, not all clients
or full Haystack 5 interoperability.

| Exit | Claim |
|---|---|
| 0 | All checks in the full local profile completed successfully on this host |
| 1 | At least one selected check failed |
| 2 | Executed checks passed, but the local profile is incomplete |

Running without `--full` omits Cargo Deny and therefore returns 2 if all other checks
pass. The local gate does not run hosted CodeQL, other host operating systems, or
release-artifact validation or optional storage runtime profiles. For SQLite, run the
[explicit native qualification commands](sqlite-durability.md#validation). For a
PostgreSQL change, also run `python3 scripts/postgres/qualify.py --docker`. For the
installed CLI workflow, run `python3 scripts/postgres/standalone.py --docker`; see the
[exact profile and external-fixture alternative](postgres-durability.md). The
baseline gate does not predict all hosted CI outcomes.

## Release build inventory and qualification limits

The tag-triggered workflows define the following build inventory. These are build
targets, not evidence that a particular release artifact was installed or executed.
Release publication remains a separate operation from PR validation.

| Artifact | Configured inventory | Current qualification boundary |
|---|---|---|
| Python wheels | CPython 3.11, 3.12, 3.13 × Linux/macOS × x86_64/aarch64: 12 combinations | The release workflow verifies every wheel's metadata/provenance and requires an installed Linux x86_64/CPython 3.12 consumer before staging; the other eleven tuples require separate runtime evidence |
| Python source distribution | One archive built with Maturin 1.15.0 | The final archive contains a checked pruned lock; qualification requires full offline locked metadata and a fresh PEP 517 installation outside the checkout before staging |
| CLI archives | `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc` | All five archives require metadata/provenance checks; the release workflow executes the downloaded Linux x86_64 archive before staging. Other targets require separate runtime evidence |
| Rust crates | Core, client, server and CLI | Publication waits for CLI qualification, but cargo publish packages source separately; the tested CLI archive does not qualify the exact crate upload bytes |

The manual `Artifact Qualification` workflow rehearses one native Linux x86_64
CLI, CPython 3.12 wheel and source archive through build, GitHub upload/download,
isolated consumers and exact-byte staging. It has read-only permissions and no
publication job. Its presence is not evidence of a successful run; retain the run
revision and file-bound receipts for that claim. See [artifact validation and
provenance](artifact-validation.md) for the source-archive repair, evidence profiles
and publication boundaries.

That manual workflow also builds one CPython 3.14 free-threaded Linux x86_64 wheel
and reuses its ordinary 3.12 wheel for two rich installed consumers. The local
selected profile covers macOS arm64 CPython 3.12.13 and 3.14.4 free-threaded using
the same receipt and consumer code. Hosted patch versions, SOABI, runtime GIL state,
native dependencies and deployment requirements are recorded per run. Selected
capabilities are typed values, async H4/H5, bounded watches, embedded memory
application, synchronous facade and bounded shared-instance/lifecycle concurrency.
Each capability process must retain unforced disabled GIL observations for the
free-threaded cell. PostgreSQL, SQLite, MCP, analytics and free-threaded source
installation remain explicitly unselected by this wheel profile.

The tag/publication matrix remains the twelve ordinary wheel tuples above. There
is no Windows wheel or Python 3.15 selection. Other interpreter, feature, backend
and platform combinations require separate execution evidence. PyO3 is not built
with `abi3`; `cp314-cp314t` is a version-specific free-threaded ABI. One interpreter's
successful build does not qualify another interpreter's wheel, and selected
concurrency runs do not certify every schedule, subinterpreter use or scaling.

## Shared application reads

`rusty-haystack-app` participates in the workspace Clippy, unit/integration and
doctest selections above. It has no Axum dependency. The server's scoped HTTP
profile is tested against actual embedded codec output over a loopback TCP
listener, with separate fixtures for body admission, deadlines, cancellation,
policy masking, cursor invalidation, and unavailable routes. These are local
behavior checks, not production authorization-policy certification or artifact
publication qualification. See [shared read contracts](shared-reads.md).

The CLI container copies `haystack-app` as a workspace member. Crates.io publication
orders core, client, app, server, then CLI, with the existing index wait between
packages. The CLI/wheel/sdist artifact matrix and receipt identity remain separate
from this source-package ordering.

## Entity mutation and feed qualification

Local workspace tests cover prepared core batches and indexes, native singleton
changes, complete-span count/byte retention, current policy disclosure, stable
receipt binding, provider-protocol failure fixtures, managed worker ownership,
and actual HTTP/client requests including a lost committed response reconciled
without resubmission. The default provider is ephemeral process memory. Retained
fixture state across service recreation is not crash recovery or disk/database
durability evidence. Multi-process commit ordering, history mutation, equipment
commands and WebSocket mutations remain unqualified and out of this slice. See
[entity mutation and feed contracts](entity-mutations.md).

The optional PostgreSQL profile adds an independently required Ubuntu job without
changing core/default feature coverage or the baseline local gate. Its documented
18.6 fixture tests qualify the selected application path at the tested revision;
two real application processes share the dataset with coherent refresh, bounded
periodic catch-up, actual local TCP interruption/reconnect, durable feed replay,
process-local subscription resync and explicit restore-epoch fencing. Other
database versions, geographic replication, power-loss behavior and backup
restoration remain separate evidence requirements.


The optional SQLite entity profile uses the bundled SQLite 3.53.2 runtime through
rusqlite 0.40.2, with a single application owner for one local file. Its separate
Ubuntu x86_64 job includes native commit/rollback and process interruption before
and after COMMIT, durable pending/final receipts, typed payload recovery, owner
locks and actual native cleanup. Native macOS arm64 can run the same commands
locally; other operating systems and architectures require separate evidence.
This does not qualify power loss, network filesystems, multiple application owners,
history storage, a SQLite CLI selector, or Python wheels containing SQLite. See
[the selected SQLite contract](sqlite-durability.md).
