# Artifact validation and provenance

The release workflows build each binary archive, wheel and source archive once, then qualify downloaded candidates and stage byte-for-byte copies. Build receipts, runtime evidence and staging records are kept separate from package files. Publication jobs download that staged directory and recheck its hashes before the existing publishing action runs.

`Artifact Qualification` is a manually dispatched rehearsal with `contents: read`, no package-registry credentials and no publication jobs. It builds one Linux x86_64 CLI archive, a CPython 3.12 wheel and the source archive. A separate job downloads those three exact, same-run artifact names outside the checkout, verifies their provenance, exercises installed consumers and stages identical bytes. A dispatch at a tag still cannot publish. The workflow must exist on the repository's default branch before GitHub accepts its first manual dispatch.

## Scope of the evidence

| Profile | Required inventory | Executed consumer checks |
| --- | --- | --- |
| `native` | One CLI archive and CPython 3.12 wheel for an explicit native target, plus the source archive | Native CLI, installed wheel and source installation |
| `binaries` | Five existing CLI targets: Linux and macOS x86_64/aarch64, Windows x86_64 | Linux x86_64 CLI |
| `python` | Twelve existing CPython 3.11/3.12/3.13 wheels for Linux/macOS x86_64/aarch64, plus the source archive | Linux x86_64 CPython 3.12 wheel and source installation |

Every file receives archive, native-header, package, version, target and provenance checks. A metadata check for another target is build-artifact evidence, not runtime qualification for that operating system, architecture or interpreter. No Windows wheel, abi3, free-threaded Python or newer runtime floor is introduced.

The isolated CLI consumer checks `haystack --version` and a two-entity Zinc-to-JSON v3 export with a `site` filter against fixed expected values. The Python consumer checks the installed distribution and module version, imports the public submodules, exercises number/ref/Zinc behavior and the public codec exception, and compares authentication derivation with independent standard-library PBKDF2/HMAC calculations. It verifies that import origins are in a newly created virtual environment while the working directory is outside the checkout. Python path overrides are cleared and the consumer runs with `-I`.

Typing evidence covers installed `__init__.pyi` and `py.typed` files, distribution file records and parsed public stub declarations. It does not claim that a full type checker has validated every annotation.

## File identity and promotion

Each uploaded candidate contains only `files/<one package file>` and `receipts/<filename>.json`. The receipt records:

- The exact clean source revision and SHA-256 of the checkout's `Cargo.lock`.
- Package name/version and the final package filename, size and SHA-256.
- Target, release/source profile, explicitly enabled features and actual Rust/Cargo versions.
- For Python artifacts, the actual Maturin version and interpreter identity; wheels also bind Python/ABI/platform tags and `Requires-Python` metadata.
- For container builders, the actual inspected image ID and immutable registry digest. The cross executable is pinned to 0.2.5 and its version is captured.
- For the repaired source archive, its distinct shipped lock hash and the exact removed lock packages.

These are build records bound to the same workflow's artifacts, not independently signed supply-chain attestations. Receipt verification cannot establish a stronger trust model than the workflow, checkout and artifact service that produced them.

Uploads fail if no files exist. Downloads name each candidate exactly and keep separate directories; there is no wildcard merge. Names include the run attempt, and downloads use the current workflow run. GitHub's artifact-service ID and service digest are retained in build job summaries and logs. That service digest describes GitHub's uploaded container and is distinct from the package-file SHA-256 in the receipt.

`scripts/release/artifacts.py verify` rejects missing/extra/duplicate tuples or files, source/lock/version/target/ABI/digest differences, unsafe archive entries and incompatible builder records. Archives are inspected before extraction; absolute or traversing paths, links, duplicate entries and oversized inventories are rejected.

Qualification writes one evidence record per file, bound to both the file digest and its build-receipt digest. `stage` requires the complete evidence set and the checks required by the selected runtime profile. It creates a fresh directory containing package files only, copies each file and checks its digest again. `check-staged` validates the complete final inventory, source, version and hashes after the next download. Existing directories are not overwritten; an interrupted attempt remains available for diagnosis.

## Source archive lock repair

Maturin 1.15.0's Cargo source generator trims workspace members while copying the original workspace lock. The unmodified archive can therefore fail full `cargo metadata --offline --locked`; shallow `--no-deps` metadata does not establish a usable locked graph.

The builder extracts one raw archive into disposable staging and checks that its initial lock equals the checkout lock. Offline Cargo metadata prunes it. The helper rejects additions, upgrades, changes to retained package/dependency/checksum records, changes to lock metadata, or removal of a package still referenced by retained entries. Full offline locked metadata must then succeed before the builder creates and seals one final archive.

Downloaded qualification extracts that sealed archive into another fresh directory, repeats full offline locked metadata, installs the pinned Maturin 1.15.0 backend into a new virtual environment, and runs pip's PEP 517 install with dependency lookup and build isolation disabled. `pyproject.toml` pins the backend and requests locked Maturin builds. The shipped lock must remain unchanged after installation. The wheel built during this source-install check is validation-only; it never replaces the separately built release wheel. Cargo dependencies must already be available locally, and Cargo stays offline for the extracted-source build. Installing the pinned backend may use the configured Python package index/cache.

## Local rehearsal

Use a clean committed checkout, Rust 1.99.0 and a trusted CPython 3.12 interpreter with Maturin 1.15.0 installed. Keep outputs outside the checkout and use a fresh directory for each attempt. A retained Cargo target/cache directory may be reused; these checks do not claim a cold build.

```bash
REPO="$PWD"
SOURCE=$(git rev-parse HEAD)
TARGET=$(rustc -vV | sed -n 's/^host: //p')
PYTHON="$REPO/.venv/bin/python"
RUN=$(mktemp -d /tmp/haystack-artifacts.XXXXXX)
cargo fetch --locked

python3 scripts/release/build.py cli --repo "$REPO" --source "$SOURCE" \
  --target "$TARGET" --bundle "$RUN/built/cli" --work "$RUN/build-cli"
python3 scripts/release/build.py wheel --repo "$REPO" --source "$SOURCE" \
  --target "$TARGET" --python "$PYTHON" --bundle "$RUN/built/wheel" --work "$RUN/build-wheel"
python3 scripts/release/build.py sdist --repo "$REPO" --source "$SOURCE" \
  --target source --python "$PYTHON" --bundle "$RUN/built/sdist" --work "$RUN/build-sdist"

# Local transfer rehearsal; a hosted run separately proves GitHub upload/download.
cp -R "$RUN/built" "$RUN/download"
python3 scripts/release/qualify.py --repo "$REPO" --source "$SOURCE" \
  --profile native --target "$TARGET" --python "$PYTHON" --input "$RUN/download" \
  --work "$RUN/consumer-work" --evidence "$RUN/evidence"
python3 scripts/release/artifacts.py stage --repo "$REPO" --source "$SOURCE" \
  --profile native --target "$TARGET" --input "$RUN/download" \
  --evidence "$RUN/evidence" --output "$RUN/publication" > "$RUN/stage.json"
python3 scripts/release/artifacts.py check-staged --repo "$REPO" --source "$SOURCE" \
  --profile native --target "$TARGET" --input "$RUN/publication" --receipt "$RUN/stage.json"
```

The helpers emit one JSON result, return nonzero on failure and retain bounded diagnostic text plus subprocess logs in the selected work directory. Run fast rejection tests with `python3 -m unittest discover -s scripts/release/tests -v`; they need no compiler, registry credentials or native build. Use `actionlint` for the three workflow definitions. Preserve failed logs and successful receipts as evidence, then remove task-owned scratch directories when they are no longer needed.

## Publication authority

The existing release and Python release workflows remain tag-push workflows. Every publication job explicitly requires a push to a `v*` tag and successful qualification. Default permissions are read-only; the existing GitHub Release job retains its contents write permission, and only the existing PyPI job retains its OIDC write permission. Crates.io and PyPI keep their existing `release` environments. GitHub Release has no release environment; this pipeline does not claim otherwise.

Crate publication now waits for binary qualification, but `cargo publish` separately packages crates from source. Testing a CLI archive does not prove the exact bytes of those crate uploads. Running qualification or staging does not authorize tag creation, a release, credential use, or publication. No package is published by the rehearsal workflow or local commands above.
