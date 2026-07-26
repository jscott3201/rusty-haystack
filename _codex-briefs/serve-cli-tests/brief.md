# Brief: automated coverage for `haystack serve` (closes #24)

**From:** orchestrator (Claude Code, agent df523645-b63f-4966-8114-24bf31d2f363)
**To:** executor (Codex lane `serve-cli-tests`)
**Repo:** rusty-haystack — Rust implementation of Project Haystack
**Issue:** https://github.com/jscott3201/rusty-haystack/issues/24

## Branch contract

- Base branch: `dev`
- Base SHA this brief was written against: **9b55e0d374cf457011c1f69745ba825bd2284307**
- New branch: `test/serve-cli-coverage`
- PR target: `dev`. **Open the PR. Do not merge it.** Merge authority is not delegated.
- Re-anchor every line number below against the SHA you actually check out. Lines move.

## The problem

`haystack-cli/tests/cli.rs` (95 lines, added in #17) spawns the real binary and covers
`export --filter`. It does not cover `serve`.

`serve` is where the bug that prompted that harness actually lived: the graph was built
without a namespace, so every spec-match filter was answered from an ontology-less graph.
That fix was verified by hand with curl, never by a test. **Reverting the `serve.rs`
namespace wiring today breaks nothing in CI.** That is the gap to close.

## Grounding

Read these before writing anything. Every claim below was checked at the SHA above.

- `haystack-cli/src/commands/serve.rs` — 112 lines, the whole command.
  - `ServeConfig` (lines 7-13): `port: u16`, `file: Option<&str>`, `users_file`, `host`,
    `demo: bool`.
  - Line 28: `DefNamespace::load_standard()` into an `Arc`, shared into every graph below.
    The comment on lines 23-27 states the invariant this brief exists to protect.
  - Line 58: `--file` path, `EntityGraph::from_grid(&grid, Some(Arc::clone(&ns)))`.
  - Line 68: `--demo` path, `EntityGraph::with_namespace(Arc::clone(&ns))`.
  - Line 78: no-args path, an empty graph that still carries the namespace.
  - Lines 41-47: format is chosen by file extension — `.trio`, `.json`, else zinc.
- `haystack-cli/tests/cli.rs` — the harness to extend. Note `CARGO_BIN_EXE_haystack` and
  the `GRID` constant (lines 15-19: one site `@s1`, one AHU `@e1`, one point `@p1`).
- `haystack-cli/Cargo.toml` — `[dev-dependencies]` is `tempfile = "3"` only.
  `haystack-client` IS a normal dependency of this crate, so a test may use it.
- `haystack-server/src/app.rs:183` — see ruling **C1**.

## Contradiction rulings

These override the issue text. The issue's citations rot; these were checked.

**C1 — "bind port 0 and read back the assigned port" is not possible.** The issue
suggests it. `haystack-server/src/app.rs:183` binds
`tokio::net::TcpListener::bind(format!("{}:{}", self.host, self.port))` and the command
never prints the resolved `local_addr()`. Passing `--port 0` therefore yields an ephemeral
port that nothing outside the process can discover.

Use the standard workaround instead: in the test, bind a `std::net::TcpListener` to
`127.0.0.1:0`, read `local_addr()?.port()`, **drop the listener**, and pass that port to
the child. There is a small race window if something else claims the port in between;
that is accepted and is far better than a hardcoded port. Do not add a
"print the bound port" change to `serve.rs` to work around this — see the scope fence.

**C2 — the startup banner is not a readiness signal.** `serve.rs:93-96` prints
`Starting Haystack HTTP server on {host}:{port}` to stderr *before*
`HaystackServer::run()` is awaited, so it is emitted before the socket is bound. Readiness
must be a poll loop against the port (connect, or an HTTP request that returns any
response), with a bounded deadline. A fixed `sleep` is not acceptable.

## Work

Extend `haystack-cli/tests/cli.rs`. Add a `serve` harness and the cases below.

### Harness requirements

- Pick a free port per C1. Every test gets its own port; they may run concurrently.
- Poll for readiness per C2, with a deadline (a few seconds is plenty) and a clear panic
  message naming the port if the deadline passes.
- **Kill the child in a guard**, so a failing assertion does not leak a server process.
  A `struct` with a `Drop` impl that calls `kill()` then `wait()` is the shape. A test
  that panics between spawn and an explicit kill must still clean up.
- Speak HTTP however is simplest and dependency-free-est. Options, in preference order:
  (1) `haystack-client`, already a dependency of this crate; (2) a raw `TcpStream` with a
  hand-written request and a substring assertion on the response body. Do **not** add
  `reqwest` as a dev-dependency — see the scope fence.

### Cases to cover

The criterion, not an enumeration: **every assertion must discriminate.** An assertion
that the response contains *some* rows passes against the "matches everything" bug this
harness exists to catch. Assert both what must be present and what must be absent.

1. `--demo`: filter `ph::Ahu` returns the AHUs and **not** the points; filter `ph::Point`
   returns the points and **not** the AHUs; filter `ph::Bogus` is rejected (the issue says
   400 — verify the actual status and assert what the server really does, do not assume).
2. `--file <path>`: same discrimination against a small fixture grid written to a
   `tempfile`. Reuse or adapt the existing `GRID` constant. Cover at least one non-default
   extension so the `serve.rs:41-47` extension dispatch is exercised.
3. No `--demo`, no `--file`: an empty graph still **accepts** a spec-match filter rather
   than erroring about a missing namespace. This is the case that regresses if
   `serve.rs:78`'s `with_namespace` is dropped.

### The test that proves the test

Before you finish: revert the namespace wiring locally (e.g. change `serve.rs:68` to
`EntityGraph::new()` or whatever the no-namespace constructor is), confirm your new tests
**fail**, then restore it and confirm they pass. Report both outcomes in `status.md` with
the actual assertion output. A test that passes both with and against the bug is not
coverage, and reporting it as coverage is the failure mode this step exists to prevent.

Do not commit the reverted state.

## Scope fences — these must not change

- `haystack-cli/src/commands/serve.rs` — **zero changes.** This is a test-only PR. If you
  believe a production change is required to make it testable, that is a BLOCKED report,
  not a change.
- `haystack-server/` — zero changes.
- `haystack-core/` — zero changes.
- `.github/workflows/` — zero changes. The new tests run under the existing
  `cargo test --workspace --exclude rusty-haystack`.
- No new `[dependencies]`. A new `[dev-dependencies]` entry is permitted **only** if you
  cannot do it with `tempfile`, `haystack-client`, and `std` — and if you add one, justify
  it in `status.md` against the toolkit's default-no rule for dependencies.
- Do not touch `haystack-cli/tests/cli.rs`'s two existing `export` tests.

## Fallback rules

Pre-authorized, so you do not round-trip on these:

- If `haystack-client` turns out to need an async runtime the test cannot cheaply provide,
  drop to a raw `TcpStream` + hand-written HTTP/1.1 request. `tokio` is already a
  dependency of this crate with `rt-multi-thread` and `macros`, so `#[tokio::test]` is
  also available.
- If the demo graph's AHU/point counts differ from what you expect, assert against the
  actual counts you measure — but assert an exact count, not "> 0".
- If `ph::Bogus` returns something other than 400, assert the real behaviour and note the
  discrepancy against the issue in `status.md`. Do not change the server to match.
- If concurrent tests prove flaky on port selection, serialize them with a mutex rather
  than reaching for a hardcoded port.

## Verification gates

Run these, in this form, and paste the **real output** into `status.md` — not a summary.

```
cargo fmt --all --check
cargo clippy --workspace --exclude rusty-haystack --all-targets -- -D warnings
cargo test -p rusty-haystack-cli
cargo test --workspace --exclude rusty-haystack
```

Then run the CLI test binary at least 5 times in a row and report whether it was stable.
A port-picking test that passes once is not evidence it is not flaky:

```
for i in 1 2 3 4 5; do cargo test -p rusty-haystack-cli --test cli || echo "FAILED run $i"; done
```

Never weaken a gate to reach green. No `#[ignore]`, no `allow`, no loosened assertion.
If a gate fails and the cause is outside your scope fence, that is a BLOCKED report.

## Stop semantics

- **Done:** tests written, all gates pass with output pasted, the revert-proof from
  "The test that proves the test" reported, branch pushed, PR opened against `dev`.
  Append `DONE` to `status.md` with the PR URL. Then stop. Do not merge. Do not widen
  scope.
- **Blocked:** append `BLOCKED` to `status.md` with the specific obstacle, what you tried,
  and the `file:line` evidence. Then stop. A blocked report is a good outcome; a confident
  wrong implementation is not.
- Git mutations must be issued as **bare single invocations**, never chained with `&&`.
  Chained git commands fail in this sandbox with `.git/index.lock: Operation not
  permitted`.

## Binding decisions — do not relitigate

- Test-only PR. `serve.rs` is correct as written; this brief exists because nothing proves
  that.
- C1 and C2 above.
- The discrimination requirement. Assertions that only prove "some rows came back" do not
  satisfy this brief.

---

## Amendment 2026-07-25 — base SHA correction

The branch contract above named base SHA `9b55e0d374cf457011c1f69745ba825bd2284307`.
That commit is **not on `dev`** — it is the head of `fix/lint-pyo3-bindings`, an open PR
(#30) that the orchestrator was working when this brief was drafted.

**Binding correction:** base off `dev` at
**59db4dd34b9e42b95a26049ac993696c76973e0b**. That is where your clone is checked out.

The grounding in this brief is unaffected. `9b55e0d` differs from `59db4dd` only in
`rusty-haystack/src/{client,graph,server}.rs`, `.github/workflows/ci.yml` and
`.gitignore` — none of which this brief cites or touches. Every `file:line` above was
read in a tree identical to `59db4dd` for the files concerned. Re-anchor anyway.
