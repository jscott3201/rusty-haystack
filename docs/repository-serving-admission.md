# Read-only repositories and explicit package admission

An embedding can serve a finite immutable repository with
`HaystackServer::with_public_repository(LibraryRepository)`. This explicitly
exposes public GET operations below `/repo/`; it is absent by default and shares
the application's request lifetime and shutdown controls. Existing `/api/`
authentication and catalog function dispatch are independent. Attaching a
repository does not admit or activate its packages.

The selected endpoints are `sys.repo::repoPing`, `sys.repo::repoSearch`,
`sys.repo::repoVersions` and `sys.repo::repoFetch`. Search uses a case-sensitive
name substring, or `*`, with alphabetical library order, total matching count
and `more` when the limit truncates results. Versions use numeric newest-first
ordering and the existing wildcard/inclusive-range grammar, filtered before
limiting. `latestStable` can differ from the newest served version. Limits are
1–64, default 64. Unknown libraries return an empty versions list; fetch returns
separate typed errors for an unknown library and an unknown version.

Metadata uses contextual JSON with `Xeto-Version: 5`; the selected Accept forms
are absent, `application/json`, `application/json;box=none` and `*/*`.
`xeto-version=5` and `xeto-filetype=json` are supported query controls (query
controls take precedence over the corresponding headers). Fetch ignores Accept
and returns the exact retained bytes as `application/xetolib`; filetype
selection is rejected for this binary operation. The profile is GET-only and
unconditional: unsupported methods, formats, box modes, conditional/range
headers, duplicate arguments and unknown controls fail explicitly. Query text
is at most 4096 bytes, with at most eight fields and 1024 bytes per value.
Errors carry version 5 and typed JSON. `repoPublish` always reports unsupported;
there is no upload, install, activation or package publication endpoint.

`LibraryRepository` owns at most 64 structurally verified packages and 16 MiB
of aggregate archive input. The same pin and identical bytes may be supplied
again; different bytes at an existing pin are a conflict. Every advertisement
and fetch comes from the same immutable record. Its SHA-256 is computed over
the exact archive bytes, never regenerated content. All configured entries are
advertised as `available`; no mutable yanking/deletion workflow or timestamps
are invented. Construction reads no filesystem paths, and serving never reloads
files. The client remains responsible for its independent transport provenance
and digest checks.

## Supported source package profile

`LibraryPackage::from_bytes(pin, bytes)` verifies a bounded ZIP source package.
Successful construction establishes archive and metadata consistency, **not**
semantic catalog admission or authenticity. The selected profile is:

- Exactly `xeto-meta.props`, followed by root `lib.xeto`, in local and central
  entry order. STORE and raw DEFLATE are supported, including UTF-8 flags and
  signed or unsigned data descriptors. Local/central sizes, names, timestamps,
  offsets, CRCs and descriptors must agree; the complete compressed stream and
  uncompressed size must match, with no trailing or unclaimed bytes.
- At most 1 MiB compressed archive input, 16 KiB expanded properties and 64 KiB
  expanded source. Actual decompression is bounded independently of declared
  sizes. There is no filesystem extraction.
- No additional files, resources, build variables, ZIP extra fields/comments,
  encryption, multiple disks, ZIP64, links/special files, duplicate entries,
  absolute paths, traversal, aliases or backslash paths. Metadata-first ordering
  and rejection of extra fields/comments are profile restrictions; valid general
  ZIP/Xeto packages can lie outside this profile.
- UTF-8 properties with trimmed surrounding whitespace, ordinary upstream
  writer escapes (`\n`, `\r`, `\t`, `\\`, `\uXXXX`, including surrogate pairs),
  and no duplicate decoded keys. Comments, continuations, unsupported escapes
  and extra fields fail explicitly. Required fields are `name`, `version`,
  `maturity` and `depends`; `doc` may be absent and defaults to empty.
- The supplied pin, metadata name and Catalog library argument agree. Source
  pragmas do not independently name their library. Package/source/dependency
  versions must be canonical three-component numeric strings: `1.2.3`, never
  `01.2.3`, wildcard or range. No signed/hashed source is rewritten. The general
  repository client's numeric normalization and query ranges remain available.
- Source metadata is limited to literal `version`, `maturity`, `depends` and
  `doc`. Missing maturity defaults to stable. Dependencies must be a list of
  exact two-field dictionaries with unique `lib` and canonical `versions`.
  The raw metadata is checked, including malformed entries, instead of relying
  on the parser's convenience list. Props dependencies must equal the declared
  source order serialized as `lib version;lib version`. Version, dependencies,
  maturity and normalized documentation must agree. Doc equality is an
  additional selected-profile rule.

The authoritative pinned Xeto contract is
[873b922451d3ef4c0c9c08ef3daa542f352d69f3](https://github.com/Project-Haystack/xeto/tree/873b922451d3ef4c0c9c08ef3daa542f352d69f3).
[Haxall's source archive writer](https://github.com/haxall/haxall/blob/aded27993c2d4834eca4b44ca55671417a1a7ea2/src/core/xetoc/fan/compiler/OutputZip.fan)
and [metadata agreement check](https://github.com/haxall/haxall/blob/aded27993c2d4834eca4b44ca55671417a1a7ea2/src/core/xetoc/fan/compiler/Parse.fan)
establish that these archives contain source, not a compiled Catalog. General
upstream archives may contain additional sources, resources and build variables;
this profile rejects them rather than ignoring meaningful contents.

## Explicit additive admission and activation

`PackageSelection::new(roots, packages, markers)` takes exact selected roots,
structurally verified packages and explicit caller-owned marker associations.
There is no network resolver, latest-version choice or automatic fetch. A
selection has at most 16 supplied packages, 16 roots, 16 dependency levels,
32 direct dependencies per package, 128 markers, 1 MiB aggregate source and
16 MiB archive input. The candidate is additionally bounded to 128 project
declarations and 2048 declared project slots across retained and new libraries,
so smaller repeated batches cannot evade the aggregate bound. Existing strict
compilation also caps inheritance at 64 levels. All supplied packages must be
reachable from the roots.
Missing dependencies may be satisfied by the existing catalog at the exact
version, including source-only libraries. Their reachable retained dependencies
are traversed under the same 16-level depth, cycle, canonical pin and raw metadata
checks as supplied packages, so admitting smaller batches cannot hide a longer
chain or stale transitive version. A retained declaration whose raw dependencies
cannot establish this supported closure fails explicitly. The traversal checks
application control at each visit and is capped at 134 distinct reachable
libraries (128 project libraries plus the six supported pinned libraries).
This is a selected-profile resource ceiling, not a general Xeto limit. Unrelated
retained libraries are preserved outside this closure; supplied packages must
still all be reachable. Missing pins, cycles, conflicting versions or unreachable
inputs fail before a candidate becomes available.

Semantic admission reuses `Catalog::with_project`: selected Dict types only,
with its existing declaration, slot, metadata and callable-context bounds.
Repository names can be broader than supported project names; the existing
project restrictions (including no underscores or `sys`/`ph` prefixes) are
reported as admission errors without renaming. Unsupported types, augmentation,
structural refinements and other unselected constructs are rejected.

Admission is additive. An existing library can only be repeated when the
catalog retains that exact package pin and archive digest; existing source-only
libraries cannot prove archive identity. Replacement and upgrades are not part
of this slice. Repeated marker associations must match existing bindings.
Package archive provenance contributes to catalog selection identity separately
from source provenance. A later explicit `with_project` replacement clears the
replaced library's package identity, even if its version is unchanged.

For an owning application:

1. Download inert bytes using `RepositoryClient` and verify its advertised or
   caller-supplied digest as appropriate.
2. Construct `LibraryPackage` values and the explicit `PackageSelection`.
3. Call `ReadService::prepare_packages` as `TrustedEmbedding`. Preparation runs
   under the application admission and worker lease, captures the current
   `Arc<ActivatedCatalog>`, and returns an opaque `PreparedPackageAdmission`.
   It performs no publication. Pure core `PackageSelection::admit` is also
   available to trusted synchronous callers; it has no application lifecycle.
4. Call `activate_prepared_packages` with the prepared value and activation
   limits. This uses the existing graph fitting, handler binding, commit fence,
   authority/session/deadline and owner-sealing checks. If any different
   observation has activated since preparation—even an identical selection—the
   result is `Conflict` and the newer catalog remains untouched.

Preparation has its own maximum duration, default and hard ceiling 60 seconds;
activation is a separate bounded call with its existing activation limits.
These APIs do not promise one deadline spanning both calls. Preparation checks
cancellation while traversing the closure and between bounded package compilation
steps; a single synchronous compiler step
is not CPU-preemptible. Prompt cancellation or dropping the awaiting future
retains the worker's registration until its actual exit. Owner shutdown seals
publication and waits for admitted workers according to existing shutdown policy.

On successful activation, catalog observation, contexts, derived namespace and
revision-bound resources move together and one wake is emitted. Failure leaves
the current observation intact. An in-flight request retains coherent old
encoding context or is rejected by existing revision checks before graph lookup;
it never combines old and new catalog semantics.

## Evidence and qualification limits

Core fixtures were independently produced with Python's standard ZIP writer;
the retained manifest records exact bytes' SHA-256 and original source. Tests
cover stored/compressed archives, descriptor forms, UTF-8 flags, malformed and
oversized inputs, metadata agreement, closure conflicts and archive identity.
A real local owned server is exercised by the PR06 client through exact-byte
fetch, two-package admission and atomic activation. Application tests cover
stale/identical observations, failed activation, coherent retained requests,
cancellation, deadline, future drop and owner close with actual worker retention.

This is local evidence for the selected profile. It does not qualify arbitrary
Xeto packages, Haxall/Fantom runtime interoperability, remote package management,
publication, authentication for repository downloads or production deployment.
Neutral repository value types now live in `haystack_core::xeto::repository` and
are re-exported by the client; their parsing errors are `RepositoryValueError`,
with explicit conversion into the client's transport-facing `RepositoryError`.
