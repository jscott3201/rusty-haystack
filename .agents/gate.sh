#!/usr/bin/env bash
# The repo's gate: everything CI enforces, runnable before a commit.
#
# Discovered by the toolkit's probe order:
#   .agents/gate.sh -> .claude/gate.sh -> make gate -> the gate section of AGENTS.md
#
# The point is that a caller — a lane, a subagent, or a person — does not have to
# learn that this workspace excludes one crate from clippy, or that the Python
# bindings need a maturin build before pytest sees them. Those are real traps:
# `cargo clippy --workspace` fails on the PyO3 crate, and `pytest` against a stale
# .so passes while the Rust change under test is not present.
#
# Note for lanes: .agents/ is read-only inside the sandbox, so a lane can run this
# but cannot edit it.

set -o pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.." || exit 1

FULL=0
[[ "${1:-}" == "--full" ]] && FULL=1

status=0
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

# CI runs these three on every PR. Keep the flags identical — a gate that lints a
# different target set than CI is worse than no gate, because it reports green on
# what CI is about to reject.
run "rustfmt" cargo fmt --all --check
run "clippy" cargo clippy --workspace --exclude rusty-haystack --all-targets -- -D warnings
run "tests" cargo test --workspace --exclude rusty-haystack

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
else
  printf '\n\033[33mskip\033[0m python bindings — no .venv (uv venv --python 3.12)\n'
fi

# Network-dependent and slow, so opt-in. CI runs it on every PR and nightly, which
# is where a newly-published advisory will surface; locally it mostly costs time.
if (( FULL )); then
  run "cargo-deny" cargo deny check
else
  printf '\n\033[33mskip\033[0m cargo-deny — run with --full\n'
fi

printf '\n'
if (( status )); then
  printf '\033[31mgate failed\033[0m\n'
else
  printf '\033[32mgate passed\033[0m\n'
fi
exit "$status"
