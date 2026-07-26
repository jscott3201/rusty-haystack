# Contributing

## Prerequisites

- **Rust 1.97.1.** CI pins this exact toolchain (`.github/workflows/ci.yml`), and the
  workspace declares `edition = "2024"` with `rust-version = "1.97"` (`Cargo.toml`).
- **[uv](https://docs.astral.sh/uv/)** and **Python 3.12**, only if you touch the Python
  bindings. CI pins the interpreter version deliberately — `pyo3` is configured without
  `abi3`, so every build is interpreter-specific.

## Build and test

```bash
cargo build --workspace --exclude rusty-haystack
cargo test  --workspace --exclude rusty-haystack
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
cargo test -p rusty-haystack-core
cargo test -p rusty-haystack-core -- test_name
```

## The gate

Before opening a PR, run the repo gate. It exists so you do not have to remember which
crate is excluded from which command:

```bash
./.agents/gate.sh          # rustfmt, clippy, tests, Python bindings
./.agents/gate.sh --full   # also cargo-deny
```

It runs CI's commands in CI's exact form. If it passes and CI does not, the gate is wrong
and that is a bug worth reporting — a gate that checks a different target set than CI is
worse than no gate, because it reports green on what CI is about to reject.

## What CI enforces

`RUSTFLAGS: "-Dwarnings"` is set for the whole workflow, so warnings fail the build.

| Job | Command |
|---|---|
| Rustfmt | `cargo fmt --all --check` |
| Clippy | `cargo clippy --workspace --exclude rusty-haystack --all-targets -- -D warnings`<br>`cargo clippy -p rusty-haystack-core --features chrono-tz --all-targets -- -D warnings` |
| Test | `cargo test --workspace --exclude rusty-haystack`<br>`cargo test -p rusty-haystack-core --features chrono-tz` |
| Python Bindings | clippy on the excluded crate, then `maturin develop` and `pytest` |
| Cargo Deny | `EmbarkStudios/cargo-deny-action@v2`, configured by `deny.toml` — advisories, licenses, bans, sources |

**The OS matrix is conditional.** Anything targeting `main` runs on Ubuntu, macOS and
Windows. Everything else — including PRs into `dev` — runs Ubuntu only. So a PR showing a
single green `Test (ubuntu-latest)` has not been checked on the other two; that happens
when the change reaches `main`. Platform-sensitive work (paths, line endings, timing)
deserves a local check on your own OS before you rely on the matrix.

The Clippy job excludes the PyO3 crate because `cargo clippy --workspace` needs an
interpreter to resolve pyo3's build script. The Python job has one and lints the crate
there. The exclusion is about *where* the lint runs, not *whether* it runs.

## Python bindings

```bash
uv venv --python 3.12
uv pip install maturin pytest
source .venv/bin/activate
maturin develop --release -m rusty-haystack/Cargo.toml
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

- **No `unsafe`.** There is none in the codebase today. It is a convention rather than a
  lint — there is no `forbid(unsafe_code)` — so it holds only if reviewers keep holding it.
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
| Request body size | `haystack-server/src/app.rs` |

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
