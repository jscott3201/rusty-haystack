# Brief: DST-aware timezone offset resolution behind a cargo feature (closes #6)

**From:** orchestrator (Claude Code, agent df523645-b63f-4966-8114-24bf31d2f363)
**To:** executor (Codex lane `tz-dst-feature`)
**Repo:** rusty-haystack — Rust implementation of Project Haystack
**Issue:** https://github.com/jscott3201/rusty-haystack/issues/6

## Branch contract

- Base branch: `dev`
- Base SHA this brief was written against: **9b55e0d374cf457011c1f69745ba825bd2284307**
- New branch: `feat/tz-dst-resolution`
- PR target: `dev`. **Open the PR. Do not merge it.** Merge authority is not delegated.
- Re-anchor every line number below against the SHA you actually check out.

## The problem

`haystack-core` maps a Haystack city-style timezone name to an IANA zone identifier
*string* and stops there. It cannot turn `("New_York", 2024-07-01)` into `-04:00` and
`("New_York", 2024-01-01)` into `-05:00`, because there is no DST rule database in the
crate at all.

`HDateTime` stores whatever offset was embedded in the source wire text, never recomputed
from `(tz_name, instant)`. So any caller constructing a `DateTime` from a naive local time
plus a tz name, or validating that an ingested offset actually matches its tz name for
that date, has to reimplement the city→IANA mapping against a separate zone database.

## Grounding

Checked at the SHA above. Read these before writing anything.

- `haystack-core/src/kinds/tz.rs` — 107 lines.
  - `static TZ_MAP: LazyLock<HashMap<String, String>>` (line 4), built by `build_tz_map`.
  - `pub fn tz_for(name: &str) -> Option<&'static str>` (line 33): city-name lookup first
    (line 35), then a scan over `TZ_MAP.values()` (line 40) for a full IANA id.
  - `pub fn tz_map() -> &'static HashMap<String, String>` (line 50).
- `haystack-core/src/kinds/datetime.rs` — `HDateTime { dt: DateTime<FixedOffset>,
  tz_name: String }`.
- `haystack-core/Cargo.toml` — `chrono = { version = "0.4", features = ["serde"] }`.
  No `chrono-tz`. See ruling **C1**.

## Contradiction rulings

**C1 — `haystack-core` has NO `[features]` section today.** The issue proposes "an
optional `chrono-tz` cargo feature" as though feature-gating were an established pattern
here. It is not: this crate has never had a cargo feature, so this PR establishes the
convention, and nothing in CI currently builds or tests any feature-gated code.

Consequences you must handle, not ignore:
- `cargo test --workspace --exclude rusty-haystack` (what CI runs, and what
  `.agents/gate.sh` runs) will **not compile** your new code with the feature off. Code
  that never compiles in CI is untested code shipped as a feature.
- You must therefore also add CI coverage. See "Work", item 4.

**C2 — the new dependency needs justifying, not assuming.** The toolkit's standing rule is
that the default answer to a new dependency is no. `chrono-tz` clears that bar — a DST
rule database is the IANA tzdata, reimplementing it locally is absurd, and `chrono-tz` is
the canonical companion crate to a dependency already present. But you must confirm and
record, in `status.md`: its current version, that it is maintained, and that its license
is compatible with this workspace's MIT license and passes `cargo deny check`. If
`cargo deny` rejects it, that is a BLOCKED report, not a `deny.toml` edit.

## Work

1. **Add the feature.** `haystack-core/Cargo.toml` gains a `[features]` section and an
   optional `chrono-tz` dependency, wired so the feature enables it. Default off.

2. **Add the resolver.** In `haystack-core/src/kinds/tz.rs`, feature-gated: a function
   resolving `(tz_name, NaiveDateTime) -> FixedOffset`, backed by the existing `tz_for()`
   mapping to look up the `chrono_tz::Tz`. The whole point is that callers do not
   reimplement the city-name mapping — so it must go through `tz_for()`, not parse the
   name itself.

   Design decisions left to you, but state your reasoning in `status.md`:
   - The return type. A naive local time can be **ambiguous** (the repeated hour when DST
     falls back) or **non-existent** (the skipped hour when DST springs forward).
     `chrono_tz` surfaces this as `LocalResult`/`MappedLocalTime` with `None`, `Single`,
     and `Ambiguous` arms. Collapsing all three into `Option<FixedOffset>` throws away the
     distinction between "no such local time" and "two valid answers" — which is exactly
     the silent-failure shape this repo has been closing out in #15/#16/#21. Prefer a
     return type that preserves it. Do not silently pick the earlier of an ambiguous pair
     without the caller being able to tell that is what happened.
   - Whether to also offer a convenience that resolves an *instant* (unambiguous by
     construction) alongside the local-time one.

3. **Tests.** Feature-gated. Cover, at minimum: a summer and a winter date for the same
   named zone yielding different offsets (this is the whole feature); a zone that does not
   observe DST; an unknown tz name; and both DST edge cases from item 2 — the ambiguous
   hour and the non-existent hour. The DST-transition tests are the ones that matter; a
   test suite that only checks two ordinary dates would pass against a naive
   implementation that ignores transitions entirely.

   Use a zone whose transition dates are stable and well known. State in `status.md` which
   zone and which instants you chose and why.

4. **CI.** `.github/workflows/ci.yml`. Add feature coverage so this code is actually
   built and tested. The cheapest correct shape is a step in an existing job running the
   test suite with the feature enabled; adding a whole job is also acceptable if you
   justify the runtime cost. Keep the toolchain pin (`1.97.1`) and the existing
   `RUSTFLAGS: "-Dwarnings"` consistent with the rest of the file.

   **Do not** use any `${{ github.event.* }}` interpolation in a `run:` block.

5. **Docs.** If a tracked doc describes the crate's cargo features or the timezone
   handling, update it. `CLAUDE.md` is gitignored (`.gitignore:12`) — do not attempt to
   ship changes to it, and do not treat its absence from your diff as an oversight.

## Scope fences — these must not change

- `HDateTime`'s struct definition and its existing public API. This PR is **additive**.
  Do not change how `HDateTime` stores or parses offsets, and do not make it re-derive
  offsets from `tz_name`. That is a behaviour change to every codec, out of scope, and
  would collide with in-flight work on codec strictness.
- The four codecs (`haystack-core/src/codecs/`) — **zero changes**. Another lane and the
  orchestrator are working there concurrently. Any edit under `codecs/` is a scope
  violation and will conflict.
- `deny.toml` — zero changes. See C2.
- Existing behaviour with the feature **off** must be byte-for-byte unchanged. `tz_for()`
  and `tz_map()` keep their current signatures and semantics.
- No changes to any other crate in the workspace.

## Fallback rules

- If `chrono-tz`'s current release is incompatible with the pinned `chrono 0.4` line,
  report the specific version conflict as BLOCKED. Do not pin `chrono` to a different
  minor to make it fit.
- If wiring the feature turns out to need `chrono-tz` non-optional to satisfy the
  resolver, that is a design failure of this brief — report BLOCKED rather than making
  the dependency unconditional.
- If adding CI feature coverage would require restructuring an existing job, add a
  separate minimal job instead and say so.
- If the crate's MSRV (`rust-version` in the workspace `Cargo.toml`, currently 1.97) is
  below what the chosen `chrono-tz` version requires, report BLOCKED with both numbers.
  Do not raise the MSRV.

## Verification gates

Run these, in this form, and paste the **real output** into `status.md` — not a summary.

```
cargo fmt --all --check
cargo clippy --workspace --exclude rusty-haystack --all-targets -- -D warnings
cargo clippy -p rusty-haystack-core --features <your-feature-name> --all-targets -- -D warnings
cargo test --workspace --exclude rusty-haystack
cargo test -p rusty-haystack-core --features <your-feature-name>
cargo deny check
```

Both the feature-off and feature-on paths must be clean. Feature-off is the one that is
easy to forget and the one every existing user gets.

Never weaken a gate to reach green. If `cargo deny check` rejects `chrono-tz`, that is
BLOCKED — not a `deny.toml` edit.

## Stop semantics

- **Done:** feature added, resolver implemented, tests passing both with and without the
  feature, CI updated, all gates pass with real output pasted, branch pushed, PR opened
  against `dev`. Append `DONE` to `status.md` with the PR URL. Then stop. Do not merge.
- **Blocked:** append `BLOCKED` to `status.md` with the obstacle, what you tried, and
  `file:line` evidence. Then stop.
- Git mutations must be issued as **bare single invocations**, never chained with `&&`.
  Chained git commands fail in this sandbox with `.git/index.lock: Operation not
  permitted`.

## Binding decisions — do not relitigate

- The feature is **default-off**. Users who do not want the tzdata payload must not get it.
- `HDateTime` is not changed. Additive only.
- The resolver goes through `tz_for()`.
- Ambiguous and non-existent local times are surfaced to the caller, not silently
  collapsed.

---

## Amendment 2026-07-25 — base SHA correction, and a live conflict in ci.yml

The branch contract above named base SHA `9b55e0d374cf457011c1f69745ba825bd2284307`.
That commit is **not on `dev`** — it is the head of `fix/lint-pyo3-bindings`, an open PR
(#30) that the orchestrator was working when this brief was drafted.

**Binding correction:** base off `dev` at
**59db4dd34b9e42b95a26049ac993696c76973e0b**. That is where your clone is checked out.

**This one does affect you.** Unlike the other grounding, `.github/workflows/ci.yml`
*was* changed by `9b55e0d`: PR #30 adds `components: clippy` to the `python` job's
toolchain step and a `Clippy` step that runs
`cargo clippy -p rusty-haystack --all-targets -- -D warnings` inside it.

So the `ci.yml` you check out at `59db4dd` does **not** contain those lines, and PR #30
will likely merge to `dev` before yours does. Work item 4 (CI feature coverage) must
therefore be written to merge cleanly alongside it:

- Put your feature-coverage step in the `clippy` or `test` job, **not** the `python` job,
  which is where #30 is editing.
- If you must touch the `python` job, expect a conflict and say so in `status.md`.
- Do not "helpfully" add the #30 changes yourself. They are another PR's diff.
