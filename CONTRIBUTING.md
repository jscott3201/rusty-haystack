# Contributing

## Prerequisites

- **Rust 1.99.0.** The repository pins normal local commands to this exact toolchain in
  `rust-toolchain.toml`. It is also the CI MSRV lane for the workspace's declared
  `rust-version = "1.99"`. A separate Ubuntu lane pins current stable Rust 1.99.0; both
  pins currently coincide and move only through reviewed changes.
- **[uv](https://docs.astral.sh/uv/)** and **Python 3.12**, only if you touch the Python
  bindings. CI pins the interpreter version deliberately — `pyo3` is configured without
  `abi3`, so every build is interpreter-specific.

## Dependency baseline

The October 2026 refresh raises the supported Rust floor to 1.99 and selects
[resolver 3](https://doc.rust-lang.org/edition-guide/rust-2024/cargo-resolver.html),
which considers package Rust-version requirements when resolving dependencies.
Workspace dependency versions live in the root manifest; members declare their own
additional features. `Cargo.lock` records the tested resolution. Change that file
intentionally with `cargo update`, then run the locked gate below.

The refresh keeps the existing dependency families, including PyO3 0.29.3, reqwest
0.13.5 and Tokio 1.53.2. The direct rustls requirement is at least 0.23.45 to address
[GHSA-2mjx-qc3c-rqvc](https://github.com/rustls/rustls/security/advisories/GHSA-2mjx-qc3c-rqvc);
its existing `ring`, `std` and `tls12` features are preserved. Older compatible
transitive families, including base64 0.22 and tungstenite 0.29, remain where their
owning dependencies require them. First-party public APIs are unchanged; the supported
Rust floor increases.

The core still has no default features, and `chrono-tz` remains optional. All direct
base64 users, including the demo, now share 0.23 with only `std` requested. The core
alone does not enable base64 SIMD; the full client/server graph now enables
`simd-unsafe` through reqwest and hyper-util. The first-party `unsafe_code = "forbid"`
policy is unchanged and does not apply to dependency internals. flate2 1.1.10 adds
default runtime detection while retaining the miniz_oxide Rust backend. Its optional
zlib-rs dependency appears in the lockfile but is not enabled in the workspace build.

The refreshed dependency graph's highest declared Rust requirement is 1.90, below
the workspace floor; some dependencies do not declare an MSRV. License policy is
unchanged: the newly resolved optional zlib-rs package uses the already allowed Zlib
license, and two existing dependencies only normalize their MIT/Apache SPDX spelling.
Python's declared support range and the release interpreter matrix are unchanged;
the existing build lanes now pin Rust 1.99.0 and Maturin 1.15.0.

## Build and test

```bash
cargo build --locked --workspace --exclude rusty-haystack
cargo test --locked --workspace --exclude rusty-haystack
```

### Why `--exclude rusty-haystack`

`rusty-haystack` is the PyO3 extension module. It is a `cdylib` built with pyo3's
`extension-module` feature, which **deliberately leaves the CPython symbols unresolved** —
they are supplied by the interpreter that imports the `.so` at runtime, not linked in at
build time.

That only matters for commands that link. `cargo check` and `cargo clippy` do not link, so
this particular failure does not reach them. `cargo build` and `cargo test` do, and without
the exclusion they fail with a wall of undefined symbols:

```
"_Py_NoneStruct", referenced from: ...
ld: symbol(s) not found for architecture arm64
error: could not compile `rusty-haystack` (lib)
```

If you see that, you dropped the flag. The crate is not skipped overall — it is built and
tested by its own job, through `maturin`, which supplies the interpreter (see
[Python bindings](#python-bindings)).

### Single crate, single test

```bash
cargo test --locked -p rusty-haystack-core
cargo test --locked -p rusty-haystack-core -- test_name
```

## The gate

Before opening a PR, run the repo gate. It exists so you do not have to remember which
crate is excluded from which command:

```bash
./.agents/gate.sh          # local checks except cargo-deny; exits 2 if otherwise green
./.agents/gate.sh --full   # complete local profile on this host
```

It runs the CI policy tests, isolated core minimal/default checks, and CI's Rust
commands, including the `chrono-tz` feature surface in both the MSRV and current-stable
lanes (currently Rust 1.99.0). It rebuilds the Python extension before testing it and
lints the PyO3 crate. Builds, lints and tests use `--locked` to validate the checked-in
graph. The policy tests require `python3` and only use its standard library.

**Read the exit status, not just the last line.** A check that could not run is never
reported as one that passed:

| Exit | Meaning |
|---|---|
| `0` | every check in the full local profile ran and passed on this host |
| `1` | something failed |
| `2` | what ran was green, but the local profile was incomplete (something was skipped) |

So `--full` with a `.venv` present is the run that can exit `0`. Without the venv the
Python bindings and their clippy pass cannot run, and the gate says so rather than
implying they were fine.

A local pass does not run hosted CodeQL, test another operating system, or validate
release artifacts. CI can therefore find failures the local gate cannot observe.
See the [support matrix](docs/support-matrix.md) for those evidence boundaries.

## What CI enforces

`RUSTFLAGS: "-Dwarnings"` is set for the whole workflow, so warnings fail the build.

| Job | Command |
|---|---|
| CI policy | `python3 -m unittest discover -s scripts/ci -p 'test_*.py' -v`, then the validated dev/main matrix plan |
| Rustfmt (Rust 1.99.0) | `cargo +1.99.0 fmt --all --check` |
| Clippy (MSRV, Rust 1.99.0) | `cargo +1.99.0 clippy --locked --workspace --exclude rusty-haystack --all-targets -- -D warnings`<br>`cargo +1.99.0 clippy --locked -p rusty-haystack-core --features chrono-tz --all-targets -- -D warnings` |
| Test (MSRV, Rust 1.99.0) | `cargo +1.99.0 test --locked --workspace --exclude rusty-haystack`<br>`cargo +1.99.0 test --locked -p rusty-haystack-core --features chrono-tz` |
| Core minimal and default features (Ubuntu, Rust 1.99.0) | Separate package-only Clippy and test invocations for `rusty-haystack-core`, with and without `--no-default-features` |
| Current stable (Ubuntu, Rust 1.99.0) | The same two Clippy and two test commands above, using `cargo +1.99.0` |
| Python Bindings (Rust 1.99.0) | clippy on the excluded crate, then `maturin develop` and `pytest` |
| Cargo Deny (Rust 1.99.0) | `cargo +1.99.0 deny --locked --all-features --manifest-path ./Cargo.toml check`, configured by `deny.toml` — advisories, licenses, bans, sources |
| CodeQL | Rust, Python, and Actions analyses through the reusable CodeQL workflow and existing query configuration |
| CI OK | Always evaluates the independent required-job inventory; every required job must succeed |

**The Rust test OS matrix is conditional.** Pushes to `main` and PRs targeting `main`
run on Ubuntu, macOS and Windows. Pushes to `dev` and PRs targeting `dev` run Ubuntu
only. Retargeting a PR triggers a new run for its current base branch. The other jobs
run on Ubuntu. A green dev run does not qualify the other operating systems.

`CI OK` requires the reusable CodeQL analysis and upload jobs to complete successfully.
GitHub separately evaluates alert severity in `Code scanning results / CodeQL`, which
can fail after those jobs succeed. Delivery must inspect that separate check when
present; `CI OK` does not prove there are no blocking alerts. See
[GitHub's code scanning results documentation](https://docs.github.com/en/code-security/how-tos/manage-security-alerts/manage-code-scanning-alerts/triage-alerts-in-pull-requests).

Missing, failed, cancelled, or skipped required jobs fail the workflow-job aggregate. CodeQL retains
its standalone weekly and manual runs; the separate Audit workflow retains its daily
and manual checks. Those scheduled checks are outside a PR's aggregate. This workflow
defines the canonical workflow-job result; repository branch-protection configuration is separate.

The Clippy job excludes the PyO3 crate and the Python job lints it instead, so the
exclusion is about *where* the lint runs, not *whether* it runs. Note that
`cargo clippy --locked --workspace` does lint that crate successfully on a normal developer machine
— verified on macOS with no virtualenv active — so if you want a single lint command
locally, drop the exclusion and use it.

## Python bindings

```bash
uv venv --python 3.12
uv pip install maturin==1.15.0 pytest
source .venv/bin/activate
maturin develop --locked --release -m rusty-haystack/Cargo.toml
pytest rusty-haystack/tests -q
```

Two traps, both of which CI works around explicitly:

- **Activate the venv; do not use `uv run` from inside `rusty-haystack/`.** That directory
  has its own `pyproject.toml`, so `uv run` builds a second `.venv` for it and then cannot
  find the `maturin` you installed.
- **Rebuild before you test.** `pytest` against a stale `.so` passes cheerfully while the
  Rust change you are testing is not in it.

## Workspace layout

```
haystack-core          types, codecs, graph, filter, ontology, xeto, auth
  ↑
haystack-client        async HTTP/WebSocket client
  ↑
haystack-server        Axum HTTP API, WebSocket watches  (also depends on core)
  ↑
haystack-cli           the `haystack` binary            (depends on all three)

rusty-haystack         PyO3 bindings, cdylib            (depends on core/client/server)
```

Directory names are unprefixed; crates.io names are not. `haystack-core/` publishes as
`rusty-haystack-core`, and `-p` takes the published name. The CLI binary is `haystack`.

## Conventions

- **Project-authored Rust forbids `unsafe`.** Every workspace member inherits the root
  `unsafe_code = "forbid"` lint, so the compiler rejects unsafe code in this workspace.
  This policy makes no claim about code inside dependencies.
- **Warning-free under `-D warnings`.** Do not reach for `#[allow]` to get there; fix the
  cause. If an allow is genuinely right, the comment must say why, and "renaming would
  break callers" is the kind of claim that needs checking before it is written down.
- **Hand-written recursive-descent parsers.** Filter, Zinc, Trio and Xeto are all
  hand-rolled. No parser-generator dependency.
- **New dependencies default to no.** Each one is a permanent obligation to track its
  advisories, license and maintenance. `cargo deny check` enforces the license and
  advisory side.
- **Tests live next to what they test.** Unit tests inline in `#[cfg(test)] mod tests`,
  integration tests in `<crate>/tests/`, benchmarks under `benches/` using `criterion`.

## Security limits

The codebase enforces hard limits on body size, parser nesting, collection sizes, watch
counts, history rows and filter depth.

**The constants are the documentation.** They are not restated here, because a copied
number goes stale silently and a wrong limit in a contributing guide is worse than no
limit at all. Find them at their definitions:

| Area | Where |
|---|---|
| Parser nesting, string and collection sizes | `haystack-core/src/codecs/zinc/parser.rs`, `codecs/json/v3.rs`, `codecs/json/v4.rs` |
| Filter depth, AST cache | `haystack-core/src/filter/parser.rs`, `graph/entity_graph.rs` |
| Xeto file size | `haystack-core/src/xeto/loader.rs` |
| SCRAM iteration ceiling | `haystack-core/src/auth.rs` |
| Watches, watched IDs, encode cache | `haystack-server/src/ws.rs` |
| History items and `hisWrite` rows | `haystack-server/src/his_store.rs`, `ops/his.rs` |
| `/api/changes` response rows | `haystack-server/src/ops/changes.rs` |
| Request body size | `haystack-server/src/app.rs` |
| Graph changelog capacity | `haystack-core/src/graph/changelog.rs` |
| Client in-flight requests, decompressed payload size | `haystack-client/src/transport/ws.rs` |

If you change one, change it at the definition and let the test suite tell you who cared.

## Pull requests

- PRs target `dev`. `dev` reaches `main` in batches.
- Because PRs do not target `main`, GitHub's `Closes #N` does not fire on merge. Reference
  the issue in the PR anyway, and close it by hand once the PR lands.
- One concern per PR. A refactor riding along with a behaviour change cannot be reverted
  or bisected independently of it.
- Say what you did not do and why. An unstated exclusion is indistinguishable from an
  oversight.

## A note on `CLAUDE.md`

`CLAUDE.md` is gitignored and machine-local. It is guidance for AI coding agents working
in a checkout, not a source of truth for the project — it is invisible to review and to
CI, so nothing catches it drifting.

This file is the tracked, reviewable version of anything that matters to a contributor.
Where the two disagree, this one and the code win.
