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
| CI policy | Ubuntu | Ubuntu | Process-level aggregate and matrix tests, workflow wiring regressions, and benchmark capture input/receipt regressions |

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

CodeQL runs once through CI on pushes and PRs and retains standalone weekly/manual
execution. The separate Audit workflow checks both branches daily and on manual
dispatch. Scheduled/manual results are not results for a particular PR run. Branch
protection is external configuration; this matrix does not claim that `CI OK` is
configured as a required branch-protection check.

## Local gate

`./.agents/gate.sh --full` runs the policy and benchmark capture-control tests,
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
| Python wheels | CPython 3.11, 3.12, 3.13 × Linux/macOS × x86_64/aarch64: 12 combinations | Builds are configured; PR pytest uses a development install on Ubuntu/3.12 and does not install these wheels |
| Python source distribution | One archive built with Maturin 1.15.0 | Archive creation alone does not qualify an extracted locked build; the known extracted locked-resolution failure remains a packaging repair item |
| CLI archives | `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc` | Five builds are configured; target-specific archive execution is not part of these release jobs |
| Rust crates | Core, client, server and CLI | PR locked workspace tests do not establish that each published package can build independently outside the checkout |

The current Python wheel workflow has no Windows wheel target. New interpreter
versions, free-threaded Python, alternate compression/TLS backends, and unlisted
feature or platform combinations are unqualified until explicit checks and execution
evidence cover them. PyO3 is not built with `abi3`; one interpreter's successful build
does not qualify another interpreter's wheel.
