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
`clippy`, `test`, `current-stable`, `python`, `deny`, `core-features`, and `codeql`.
It runs with `always()` and accepts only `success` for every required job. Missing
jobs, malformed or duplicate JSON keys, failure, cancellation, skip, unknown profiles,
or an unexpected planned OS inventory fail closed. Job results enter through an
environment variable, never interpolation into shell source. There are no optional
jobs in either profile; the only platform exclusion is macOS/Windows in the documented
dev test matrix.

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
and Cargo Deny. Python checks require the repository `.venv` with CPython 3.12,
Maturin 1.15.0 and pytest; the extension is rebuilt before pytest. A missing `.venv`
is recorded as incomplete, while an existing but invalid environment is a failure.

| Exit | Claim |
|---|---|
| 0 | All checks in the full local profile completed successfully on this host |
| 1 | At least one selected check failed |
| 2 | Executed checks passed, but the local profile is incomplete |

Running without `--full` omits Cargo Deny and therefore returns 2 if all other checks
pass. The local gate does not run hosted CodeQL, other host operating systems, or
release-artifact validation. It does not predict all hosted CI outcomes.

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

The current Python wheel workflow has no Windows wheel target. New interpreter
versions, free-threaded Python, alternate compression/TLS backends, and unlisted
feature or platform combinations are unqualified until explicit checks and execution
evidence cover them. PyO3 is not built with `abi3`; one interpreter's successful build
does not qualify another interpreter's wheel.

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
