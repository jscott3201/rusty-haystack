#!/usr/bin/env bash
# The repo's local gate: the selected checks on this host, before a commit.
# Hosted CodeQL, other operating systems, and release artifacts are separate.
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
#   0  every check in the full local profile ran and passed
#   1  something failed
#   2  what ran was green, but the local profile was incomplete
#
# Exit 2 exists because this script used to print "gate passed" after skipping the
# Python bindings entirely when no .venv was present (issue #47). A caller reading
# only $? could not tell the difference, which is the same silent-pass failure this
# gate is meant to catch in the code.
#
# See docs/support-matrix.md for the exact local and hosted coverage boundaries.

set -o pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.." || exit 1
export RUSTFLAGS="-Dwarnings"

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
# Each block below cites the CI job it mirrors so drift is visible in review.
run "CI policy and workflow wiring" python3 -m unittest discover -s scripts/ci -p 'test_*.py' -v  # ci.yml: jobs.ci-policy
run "benchmark capture controls" python3 -m unittest discover -s scripts/bench -p 'test_capture.py' -v  # ci.yml: jobs.ci-policy
run "artifact provenance and staging controls" python3 -m unittest discover -s scripts/release/tests -v  # ci.yml: jobs.ci-policy
run "rustfmt (MSRV 1.99.0)" cargo +1.99.0 fmt --all --check  # ci.yml: jobs.fmt
run "clippy (MSRV 1.99.0)" \
  cargo +1.99.0 clippy --locked --workspace --exclude rusty-haystack --all-targets -- -D warnings  # ci.yml: jobs.clippy
run "tests (MSRV 1.99.0)" \
  cargo +1.99.0 test --locked --workspace --exclude rusty-haystack  # ci.yml: jobs.test

# The chrono-tz feature is default-off, so the commands above never compile the
# timezone paths. CI checks them separately and so must this.
run "clippy (MSRV 1.99.0, chrono-tz)" \
  cargo +1.99.0 clippy --locked -p rusty-haystack-core --features chrono-tz --all-targets -- -D warnings  # ci.yml: jobs.clippy
run "tests (MSRV 1.99.0, chrono-tz)" \
  cargo +1.99.0 test --locked -p rusty-haystack-core --features chrono-tz  # ci.yml: jobs.test

# Separate Cargo invocations qualify core without workspace feature unification.
# Default is currently empty, but both contracts must survive future changes.
run "clippy (core minimal, MSRV 1.99.0)" \
  cargo +1.99.0 clippy --locked -p rusty-haystack-core --no-default-features --all-targets -- -D warnings  # ci.yml: jobs.core-features
run "tests (core minimal, MSRV 1.99.0)" \
  cargo +1.99.0 test --locked -p rusty-haystack-core --no-default-features  # ci.yml: jobs.core-features
run "clippy (core default, MSRV 1.99.0)" \
  cargo +1.99.0 clippy --locked -p rusty-haystack-core --all-targets -- -D warnings  # ci.yml: jobs.core-features
run "tests (core default, MSRV 1.99.0)" \
  cargo +1.99.0 test --locked -p rusty-haystack-core  # ci.yml: jobs.core-features

# The root toolchain keeps normal development on the MSRV lane. CI also carries
# one exact current-stable Ubuntu lane, mirrored here without multiplying OSes.
# The two pins coincide at 1.99.0 until a later reviewed stable update.
run "clippy (current stable 1.99.0)" \
  cargo +1.99.0 clippy --locked --workspace --exclude rusty-haystack --all-targets -- -D warnings  # ci.yml: jobs.current-stable
run "clippy (current stable 1.99.0, chrono-tz)" \
  cargo +1.99.0 clippy --locked -p rusty-haystack-core --features chrono-tz --all-targets -- -D warnings  # ci.yml: jobs.current-stable
run "tests (current stable 1.99.0)" \
  cargo +1.99.0 test --locked --workspace --exclude rusty-haystack  # ci.yml: jobs.current-stable
run "tests (current stable 1.99.0, chrono-tz)" \
  cargo +1.99.0 test --locked -p rusty-haystack-core --features chrono-tz  # ci.yml: jobs.current-stable

# The PyO3 crate is excluded above because it needs a Python interpreter to link.
# It is the crate users actually execute, so skipping it silently would hide real
# breakage. A configured but invalid venv is a failure; only a wholly absent venv
# gets the gate's "incomplete" result.
if [[ -e .venv || -L .venv ]]; then
  venv_dir="$PWD/.venv"
  venv_python="$venv_dir/bin/python"
  venv_maturin="$venv_dir/bin/maturin"
  venv_pytest="$venv_dir/bin/pytest"
  python_env_valid=1

  python_env_fail() {
    printf '\033[31mFAIL\033[0m python environment — %s\n' "$1"
    status=1
    python_env_valid=0
  }

  printf '\n\033[1m== python environment ==\033[0m\n'
  if [[ ! -d "$venv_dir" ]]; then
    python_env_fail ".venv is not a directory"
  fi
  for tool in python maturin pytest; do
    tool_path="$venv_dir/bin/$tool"
    if [[ ! -x "$tool_path" ]]; then
      python_env_fail "$tool_path is missing or not executable; recreate it with 'uv venv --python 3.12' and 'uv pip install maturin==1.15.0 pytest'"
    fi
  done

  if (( python_env_valid )); then
    if ! python_version="$("$venv_python" -c 'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")' 2>/dev/null)"; then
      python_env_fail "$venv_python could not report its interpreter version"
    elif [[ "$python_version" != "3.12" ]]; then
      python_env_fail "expected Python 3.12, found Python $python_version; recreate .venv with 'uv venv --python 3.12'"
    fi
  fi

  if (( python_env_valid )); then
    if ! maturin_version="$("$venv_maturin" --version 2>/dev/null)"; then
      python_env_fail "$venv_maturin could not report its version"
    elif [[ "$maturin_version" != "maturin 1.15.0" ]]; then
      python_env_fail "expected maturin 1.15.0, found '$maturin_version'; run 'uv pip install --python .venv/bin/python maturin==1.15.0'"
    fi
  fi

  if (( python_env_valid )); then
    if ! pytest_version="$("$venv_pytest" --version 2>/dev/null)"; then
      python_env_fail "$venv_pytest is not usable; run 'uv pip install --python .venv/bin/python pytest'"
    elif [[ "$pytest_version" != pytest\ * ]]; then
      python_env_fail "$venv_pytest returned an unexpected version string: '$pytest_version'"
    fi
  fi

  if (( python_env_valid )); then
    export VIRTUAL_ENV="$venv_dir"
    export PATH="$venv_dir/bin:$PATH"
    export PYO3_PYTHON="$venv_python"
    hash -r
    for tool in python maturin pytest; do
      resolved="$(command -v "$tool" || true)"
      expected="$venv_dir/bin/$tool"
      if [[ "$resolved" != "$expected" ]]; then
        python_env_fail "$tool resolved to '${resolved:-none}', expected '$expected'"
      fi
    done
  fi

  if (( python_env_valid )); then
    printf '\033[32mok\033[0m  python environment — Python %s, %s, %s\n' \
      "$python_version" "$maturin_version" "$pytest_version"
    printf '\n\033[1m== python bindings ==\033[0m\n'
    if "$venv_maturin" develop --locked --release -m rusty-haystack/Cargo.toml \
      && "$venv_pytest" rusty-haystack/tests -q; then
      printf '\033[32mok\033[0m  python bindings\n'
    else
      printf '\033[31mFAIL\033[0m python bindings\n'
      status=1
    fi
    # CI lints this crate here rather than in the Clippy job, because that job
    # excludes it. Running it anywhere else would leave it unlinted entirely.
    run "clippy (pyo3, MSRV 1.99.0)" \
      cargo +1.99.0 clippy --locked -p rusty-haystack --all-targets -- -D warnings  # ci.yml: jobs.python
  else
    printf '\033[31mFAIL\033[0m python bindings and PyO3 clippy — invalid .venv\n'
  fi
else
  skip "python bindings" "no .venv (uv venv --python 3.12)"
  skip "clippy (pyo3)" "needs the venv above"
fi

# Network-dependent, so opt-in locally. CI runs it on every dev/main push and PR.
if (( FULL )); then
  # cargo-deny-action@v2 runs `check` with `--locked --all-features` and the root
  # manifest. A bare `cargo deny check` inspects a narrower graph, so a crate
  # pulled in only by a non-default feature — `chrono-tz`, here — could violate
  # an advisory in CI while this printed green.
  run "cargo-deny (MSRV 1.99.0)" \
    cargo +1.99.0 deny --locked --all-features --manifest-path ./Cargo.toml check  # ci.yml: jobs.deny
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
  # that ran were green but the full local profile was incomplete.
  # `${skipped[*]}` joins on the FIRST character of IFS only, so `IFS=', '`
  # renders "a,b" rather than "a, b". Built explicitly instead.
  joined="${skipped[0]}"
  for s in "${skipped[@]:1}"; do joined+=", $s"; done
  printf '\033[33mgate incomplete\033[0m — %d check(s) did not run: %s\n' \
    "${#skipped[@]}" "$joined"
  printf 'Everything that ran passed. To complete the local profile: create the venv, then --full\n'
  exit 2
fi
printf '\033[32mlocal gate passed\033[0m — every full local-profile check ran on this host\n'
printf 'Hosted CodeQL, other operating systems, and release artifacts require separate evidence.\n'
exit 0
