# Artifact validation and provenance

The release workflows build each binary archive, wheel and source archive once, then qualify downloaded candidates and stage byte-for-byte copies. Build receipts, runtime evidence and staging records are kept separate from package files. Publication jobs download that staged directory and recheck its hashes before the existing publishing action runs.

`Artifact Qualification` is a manually dispatched rehearsal with `contents: read`, no package-registry credentials and no publication jobs. Its three-row build matrix constructs one Linux x86_64 CLI archive, a CPython 3.12 wheel and the source archive. A separate producer constructs a CPython 3.14 free-threaded wheel. Consumers download exact successful producer artifact IDs outside the checkout. The original native consumer verifies and stages the CLI/wheel/source inventory; two selected-wheel consumers reuse the ordinary wheel and qualify the free-threaded wheel. A dispatch at a tag still cannot publish. The workflow must exist on the repository's default branch before GitHub accepts its first manual dispatch.

## Scope of the evidence

| Profile | Required inventory | Executed consumer checks |
| --- | --- | --- |
| `native` | One CLI archive and CPython 3.12 wheel for an explicit native target, plus the source archive | Native CLI, installed wheel and source installation |
| `binaries` | Five existing CLI targets: Linux and macOS x86_64/aarch64, Windows x86_64 | Linux x86_64 CLI |
| `python` | Twelve existing CPython 3.11/3.12/3.13 wheels for Linux/macOS x86_64/aarch64, plus the source archive | Linux x86_64 CPython 3.12 wheel and source installation |
| `selected-wheel` | One CPython 3.12 ordinary (`cp312-cp312`) or 3.14 free-threaded (`cp314-cp314t`) wheel, on Linux x86_64 or macOS arm64 | Complete installed typed-value/static, async H4/H5, bounded watch, embedded memory application, synchronous facade and bounded concurrency consumers; native linkage and deployment requirements |

Every file receives archive, native-header, package, version, target and provenance checks. A metadata check for another target is build-artifact evidence, not runtime qualification for that operating system, architecture or interpreter. The selected profile adds no release/publication matrix entries, Windows wheel, stable ABI or newer Python floor. The version-specific free-threaded wheel has interpreter tag `cp314` and ABI tag `cp314t`; these are intentionally distinct.

The isolated CLI consumer checks `haystack --version` and a two-entity Zinc-to-JSON v3 export with a `site` filter against fixed expected values. The Python consumer checks the installed distribution and module version, imports the public submodules, exercises number/ref/Zinc behavior and the public codec exception, and compares authentication derivation with independent standard-library PBKDF2/HMAC calculations. It verifies that import origins are in a newly created virtual environment while the working directory is outside the checkout. Python path overrides are cleared and the consumer runs with `-I`.

Release typing evidence covers the complete installed root/submodule `.pyi` set and `py.typed`, distribution file records, exact equality with the current source file SHA-256 map and parsed public stub declarations. `scripts/python-values/validate.py` runs positive and negative installed consumers with Pyright 1.1.413; the selected profile requires these checks. Neither check claims every annotation has been exhaustively validated.

## Selected native wheel qualification

The selected profile pins Rust 1.99.0, Maturin 1.15.0 and the lockfile's PyO3 0.29.3 / pyo3-async-runtimes 0.29.0. Local macOS arm64 qualification selects CPython 3.12.13 and 3.14.4 free-threaded. The hosted Linux x86_64 jobs select 3.12 and 3.14t and record their actual patch versions. Every consumer must match its builder's exact interpreter version. A successful result applies to that wheel digest, source, target, interpreter and default binding feature selection.

Each runtime consumer observes its own interpreter before package import, after each public/native submodule and selected standard-library dependency import, and after the consumer finishes. Free-threaded evidence requires `Py_GIL_DISABLED`, the `cpython-314t` extension ABI, and `sys._is_gil_enabled() == False` at every point. A missing introspection result, enabled GIL, `PYTHON_GIL` environment override or `-X gil` override fails qualification. Ordinary 3.12 records its lack of the newer runtime introspection API. An import-only probe does not substitute for these same-process capability observations.

The consumer verifies every installed wheel package member against its archive digest before and after execution, including the native extension, private Python helpers and complete typing inventory. Each subprocess has a 300-second bound; on POSIX a timed-out consumer and its child fixture process group are stopped together. Runtime checks cover same-instance mutation/read, mutable input dictionaries and lists, atomic multi-tag graph snapshots, returned-copy independence, close during admitted reads and repeated application creation/destruction. Mutable `HDict`, `HList` and `HGrid` calls can reject overlapping PyO3 borrows with `RuntimeError`; this is recorded separately from successful mutations. These checks do not claim that those wrappers queue simultaneous writes or qualify arbitrary Python objects, subinterpreters, performance scaling or all possible schedules.

The separate first-party watch executable is built from the same clean source with a recorded compiler context, command and binary digest. Its receipt and source identity are bound to the consumer result; it is a test dependency, not a wheel member. macOS qualification records system-library linkage, Mach-O deployment minimum and SDK. Linux records ELF dependencies and required glibc symbol versions. Unexpected non-system libraries, explicit libpython linkage, search paths, and understated deployment tags fail.

The final evidence records passed capabilities separately from unselected PostgreSQL, SQLite, MCP, analytics, free-threaded source installation and other interpreter/platform cells. The original ordinary Linux source-archive PEP 517 installation remains a separate required rehearsal. `selected_wheel.py check` rejects altered receipt/artifact/fixture identities, missing capabilities or runtime observations, mismatched installed members and missing linkage evidence. These receipts describe observed selected runs; they are not a universal free-thread-safety certification.

## File identity and promotion

Wheel and source archives exclude and reject `__pycache__`, `.pyc` and `.pyo` files. Builder bytecode contains interpreter/path-dependent state and may be rewritten on installation; it cannot stand in for the shipped Python sources. Fresh installer-generated caches are outside the sealed member inventory.

Each uploaded candidate contains only `files/<one package file>` and `receipts/<filename>.json`. The receipt records:

- The exact clean source revision and SHA-256 of the checkout's `Cargo.lock`.
- Package name/version and the final package filename, size and SHA-256.
- Target, release/source profile, explicitly enabled features and actual Rust/Cargo versions, selected executable paths/hashes and the repository working directory. The build records the workspace profiles, admitted native inputs and allowed linker-only Cargo configuration hashes.
- For Python artifacts, the actual Maturin version and interpreter executable/digest, patch version, architecture, SOABI, extension suffix, build/runtime GIL state and typing-stub digest; wheels also bind Python/ABI/platform tags and `Requires-Python` metadata.
- For container builders, the actual inspected image ID and immutable registry digest. The cross executable is pinned to 0.2.5 and its version is captured.
- For the repaired source archive, its distinct shipped lock hash and the exact removed lock packages.

These are build records bound to the same workflow's artifacts, not independently signed supply-chain attestations. Receipt verification cannot establish a stronger trust model than the workflow, checkout and artifact service that produced them.

Uploads fail if no files exist. Their names include the producing run attempt to preserve prior evidence. Every matrix row emits a distinct job-output key containing its upload ID; empty outputs are skipped and no two rows own the same key. The consumer validates the exact set of successful producer jobs and unique numeric IDs before downloading by those IDs from the current workflow run. It never reconstructs names from its own retry attempt. Candidate directories stay separate; there is no wildcard fallback. Missing IDs or partial producer output fail the strict inventory checks. A downstream retry can reuse successful earlier producer IDs; unavailable rerun outputs fail closed. The manual workflow exercises the same matrix-output design, and hosted downstream-rerun evidence is a separate check from local fixtures. GitHub's artifact-service ID and service digest are retained in job outputs, build summaries, logs and the consumer ID record. That service digest describes GitHub's uploaded container and is distinct from the package-file SHA-256 in the receipt.

`scripts/release/artifacts.py verify` rejects missing/extra/duplicate tuples or files, source/lock/version/target/ABI/digest differences, unsafe archive entries and incompatible builder records. Archives are inspected before extraction; absolute or traversing paths, links, duplicate entries and oversized inventories are rejected.

Qualification writes one evidence record per file, bound to both the file digest and its build-receipt digest. `stage` requires the complete evidence set and the checks required by the selected runtime profile. It creates a fresh directory containing package files only, copies each file and checks its digest again. `check-staged` validates the complete final inventory, source, version and hashes after the next download. Existing directories are not overwritten; an interrupted attempt remains available for diagnosis.

Build helpers resolve Rust/Cargo 1.99.0 in the repository build context, record the selected executable hashes and explicitly use those executables for compilation. They reject hidden compiler, wrapper, Rust flag and Cargo profile selectors. Linker-only Cargo configuration and supported native inputs are recorded; unknown Cargo configuration is rejected without echoing values. Relative target directories are anchored to the build repository, so an unrelated caller-side cache cannot supply the packaged executable. Python workflow builders select and record their compiler before Maturin runs. Cross uses the pinned selected host toolchain mounted by the cross builder, with its image digest and cross version recorded. Extracted-source metadata and PEP 517 installation receive their own selected compiler context. These are bounded input-admission records, not a claim of a hermetic build.

## Source archive lock repair

Maturin 1.15.0's Cargo source generator trims workspace members while copying the original workspace lock. The unmodified archive can therefore fail full `cargo metadata --offline --locked`; shallow `--no-deps` metadata does not establish a usable locked graph.

The builder extracts one raw archive into disposable staging and checks that its initial lock equals the checkout lock. Offline Cargo metadata prunes it. The helper permits package removal and removal of existing dependency edges only. It rejects added packages or edges, upgrades, changes to retained identity/version/source/checksum or other package metadata, changes to lock metadata, and removal of a package still referenced by the final entries. The observed transformation removes the CLI/demo packages, `clap_derive` and `strsim`, as well as unused edges from retained `clap`/`clap_builder` entries. An edge may disappear even when its destination remains reachable through another package. Full offline locked metadata must then succeed before the builder creates and seals one final archive.

Maturin relocates `pyproject.toml` and the explicit `rusty_haystack/` Python package to the archive root. The sole canonical root stub is `rusty-haystack/rusty_haystack/__init__.pyi`; actual submodule stubs and the package's `py.typed` marker live alongside it. Before sealing, the archive builder verifies the complete relocated typing inventory and package loader against the current checkout, without generating or copying stubs. Missing, stale, extra or symlinked typing files fail. Wheel and sdist receipts record a `typing_files_sha256` map, and the installed consumer checks every entry so a source install cannot pass with missing or stale submodule typing.

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

The helpers emit one JSON result, return nonzero on failure and retain bounded diagnostic text plus subprocess logs in the selected work directory. The retained tests cover compiler/configuration rejection, caller-relative stale output, removal-only lock transformations, producer attempt 1 with consumer attempt 2, incomplete producer output, and mutations of publication guards, qualification dependencies, package-only staging paths and manual-workflow authority. Run fast rejection tests with `python3 -m unittest discover -s scripts/release/tests -v`; they need no compiler, registry credentials or native build. Use `actionlint` for the three workflow definitions. Preserve failed logs and successful receipts as evidence, then remove task-owned scratch directories when they are no longer needed.

To qualify a selected wheel locally, use the same clean-source builder with a trusted ordinary or free-threaded interpreter containing Maturin 1.15.0. Build in a separate environment for each ABI. Do not pass the explicit-libpython linker flags used for native Rust test executables into candidate builds. After copying that one bundle unchanged to an external download directory, run:

```bash
# TARGET is aarch64-apple-darwin or x86_64-unknown-linux-gnu.
# ABI is cp312 or cp314t; PYTHON selects the matching trusted interpreter.
python3 scripts/release/selected_wheel.py fixture --repo "$REPO" --source "$SOURCE" \
  --target "$TARGET" --work "$RUN/fixture-work" --output "$RUN/fixture.json"
python3 scripts/release/selected_wheel.py qualify --repo "$REPO" --source "$SOURCE" \
  --target "$TARGET" --abi "$ABI" --python "$PYTHON" --pyright "$(command -v pyright)" \
  --input "$RUN/download-wheel" --fixture-receipt "$RUN/fixture.json" \
  --work "$RUN/selected-work" --evidence "$RUN/selected-evidence.json"
python3 scripts/release/selected_wheel.py check --repo "$REPO" --source "$SOURCE" \
  --target "$TARGET" --abi "$ABI" --input "$RUN/download-wheel" \
  --fixture-receipt "$RUN/fixture.json" --evidence "$RUN/selected-evidence.json"
```

## Publication authority

The existing release and Python release workflows remain tag-push workflows. Every publication job explicitly requires a push to a `v*` tag and successful qualification. Default permissions are read-only; the existing GitHub Release job retains its contents write permission, and only the existing PyPI job retains its OIDC write permission. Crates.io and PyPI keep their existing `release` environments. GitHub Release has no release environment; this pipeline does not claim otherwise.

Crate publication now waits for binary qualification, but `cargo publish` separately packages crates from source. Testing a CLI archive does not prove the exact bytes of those crate uploads. Running qualification or staging does not authorize tag creation, a release, credential use, or publication. No package is published by the rehearsal workflow or local commands above.
