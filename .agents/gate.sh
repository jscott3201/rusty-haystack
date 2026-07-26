#!/usr/bin/env bash
# The repo's gate: everything CI enforces, runnable before a commit.
#
# Discovered by the toolkit's probe order:
#   .agents/gate.sh -> .claude/gate.sh -> make gate -> the gate section of AGENTS.md
#
# The point is that a caller — a lane, a subagent, or a person — does not have to
# learn that this workspace excludes one crate from clippy, or that the Python
# bindings need a maturin build before pytest sees them. `pytest` against a stale
# .so passes cheerfully while the Rust change under test is not in it.
#
# Exit status is the contract, and it has three values because "passed" and
# "everything I could run passed" are different claims:
#
#   0  every CI check ran and passed        <- the only one that predicts CI
#   1  something failed
#   2  what ran was green, but the run was not CI-equivalent
#
# Exit 2 exists because this script used to print "gate passed" after skipping the
# Python bindings entirely when no .venv was present (issue #47). A caller reading
# only $? could not tell the difference, which is the same silent-pass failure this
# gate is meant to catch in the code.
#
# Note for lanes: .agents/ is read-only inside the sandbox, so a lane can run this
# but cannot edit it.

set -o pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.." || exit 1

FULL=0
[[ "${1:-}" == "--full" ]] && FULL=1

status=0
skipped=()

run() {
  local label="$1"; shift
  printf '\n\033[1m== %s ==\033[0m\n' "$label"
  if "$@"; then
    printf '\033[32mok\033[0m  %s\n' "$label"
  else
    printf '\033[31mFAIL\033[0m %s\n' "$label"
    status=1
  fi
}

# A check that could not run is recorded, never just printed. The whole point is
# that the exit status distinguishes "everything passed" from "everything I was
# able to run passed" — a caller reading only $? must not confuse the two.
skip() {
  printf '\n\033[33mskip\033[0m %s — %s\n' "$1" "$2"
  skipped+=("$1")
}

# Keep the flags identical to CI's — a gate that lints a different target set than
# CI is worse than no gate, because it reports green on what CI is about to reject.
# Each block below cites the CI line it mirrors so drift is visible in review.
run "rustfmt" cargo fmt --all --check                                    # ci.yml:30
run "clippy" cargo clippy --workspace --exclude rusty-haystack --all-targets -- -D warnings  # ci.yml:42
run "tests" cargo test --workspace --exclude rusty-haystack              # ci.yml:65

# The chrono-tz feature is default-off, so the commands above never compile the
# timezone paths. CI checks them separately and so must this.
run "clippy (chrono-tz)" \
  cargo clippy -p rusty-haystack-core --features chrono-tz --all-targets -- -D warnings  # ci.yml:43
run "tests (chrono-tz)" cargo test -p rusty-haystack-core --features chrono-tz           # ci.yml:66

# The PyO3 crate is excluded above because it needs a Python interpreter to link.
# It is the crate users actually execute, so skipping it silently would hide real
# breakage — build and test it whenever a venv is present, and say so when not.
if [[ -x .venv/bin/python ]]; then
  printf '\n\033[1m== python bindings ==\033[0m\n'
  # shellcheck disable=SC1091
  source .venv/bin/activate
  if maturin develop --release -m rusty-haystack/Cargo.toml -q && pytest rusty-haystack/tests -q; then
    printf '\033[32mok\033[0m  python bindings\n'
  else
    printf '\033[31mFAIL\033[0m python bindings\n'
    status=1
  fi
  # CI lints this crate here rather than in the Clippy job, because that job
  # excludes it. Running it anywhere else would leave it unlinted entirely.
  run "clippy (pyo3)" cargo clippy -p rusty-haystack --all-targets -- -D warnings  # ci.yml:94
else
  skip "python bindings" "no .venv (uv venv --python 3.12)"
  skip "clippy (pyo3)" "needs the venv above"
fi

# Network-dependent and slow, so opt-in. CI runs it on every PR and nightly, which
# is where a newly-published advisory will surface; locally it mostly costs time.
if (( FULL )); then
  run "cargo-deny" cargo deny check                                       # ci.yml:110
else
  skip "cargo-deny" "run with --full"
fi

printf '\n'
if (( status )); then
  printf '\033[31mgate failed\033[0m\n'
  exit 1
fi
if (( ${#skipped[@]} )); then
  # Deliberately not "passed", and deliberately not 0. Exit 2 means the checks
  # that ran were green but the run was not CI-equivalent, so a caller cannot
  # treat it as evidence CI will pass.
  printf '\033[33mgate incomplete\033[0m — %d check(s) did not run: %s\n' \
    "${#skipped[@]}" "$(IFS=', '; echo "${skipped[*]}")"
  printf 'Everything that ran passed. For a CI-equivalent run: create the venv, then --full\n'
  exit 2
fi
printf '\033[32mgate passed\033[0m — every CI check ran\n'
exit 0
