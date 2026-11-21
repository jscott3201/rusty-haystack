# Read-only Xeto repository client

`haystack_client::repository::RepositoryClient` calls the four pinned Xeto 5
repository functions: `sys.repo::repoPing`, `repoSearch`, `repoVersions`, and
`repoFetch`. It uses qualified GET paths, bare scalar query parameters, and
`Xeto-Version: 5`. Metadata uses contextual `application/json;box=none` and
explicit repository records. Fetch returns the exact `application/xetolib` body.
The client is separate from the existing H4 grid transport.

```rust,no_run
use haystack_client::repository::{LibraryPin, RepositoryClient};
use tokio_util::sync::CancellationToken;

# async fn example() -> Result<(), haystack_client::repository::RepositoryError> {
let repository = RepositoryClient::new("https://example.com/xeto/")?;
let cancel = CancellationToken::new();
let pin = LibraryPin::new("example.equipment", "1.2.0".parse()?)?;
let versions = repository.versions(pin.lib(), None, None, &cancel).await?;
let advertised = versions.iter().find(|version| version.pin == pin);
let download = repository.fetch(&pin, advertised.map(Into::into), None, &cancel).await?;
// Retain download.bytes and its provenance for a separately owned review/admission step.
assert_eq!(download.pin, pin);
# Ok(())
# }
```

Fetch performs no implicit version lookup. `versions()` returns immutable
`RepoAdvertisement` records, binding the decoded metadata to its request/final
source URLs and redirects. Fetch accepts `MetadataEvidence::Observed` for these
records, or `MetadataEvidence::Supplied` for a plain caller-created `RepoLibVersion`.
Supplying metadata is optional; when supplied, it must match the exact requested
library and version. Mirror downloads are supported: the resulting
`metadata_source` retains repository A independently of the artifact `source` at B.
Unbound supplied metadata has no observed source and is never labeled as one.

Library versions have exactly three unsigned decimal components. Numeric comparison puts `1.10.0`
ahead of `1.9.0`; leading zeroes normalize to their numeric spelling. Library names
use lowercase ASCII letters and digits, with dots/underscores only between those
characters; each dotted section starts with a lowercase letter. SemVer
prerelease/build syntax is rejected. Version lists must already be strictly newest
first, with no duplicate or mismatched identities. Search preserves the server's
relevance order. Unknown-library version queries return the server's empty list.

Dependency metadata preserves unknown (`None`) versus explicitly empty lists.
`DependencyVersions` accepts three decimal or lowercase `x` segments and inclusive
lower–upper ranges, including whitespace around the dash. It supports matching a
known version, not dependency resolution. Every concrete segment is checked for
integer overflow. Availability remains the repository's mutable advertisement:
`available`, `yanked`, or `deleted`; exact yanked pins remain requestable. No
selection or upgrade policy is inferred from availability or maturity.

Every download records the requested pin, optional verification metadata, its
observed source when present, the artifact request/final source URLs and redirect
count, exact bytes, and computed SHA-256. Digests use `sha256:` plus unpadded base64url. Every supplied advertised or caller digest
must match; malformed algorithms/encoding and mismatches fail. `Integrity`
distinguishes `Unverified`, `Advertised`, `Caller`, `AdvertisedAndCaller`,
`SuppliedMetadata`, and `SuppliedMetadataAndCaller`. The latter two identify hashes
from unbound caller-supplied metadata; `Advertised` refers to the separately retained
metadata source, even when the artifact is downloaded from another repository.
A computed digest by itself supplies byte identity, not independent authenticity.
A repository digest comparison verifies that repository claim, not its trustworthiness.

The default configuration requires HTTPS with certificate and hostname verification.
An additional CA bundle is optional. HTTP needs explicit opt-in and a loopback IP
endpoint. The client sends no credentials, cookies or proxy requests, accepts no
injected HTTP client, and disables automatic retries and content decompression.
It requests `Accept-Encoding: identity` and rejects unexpected content encoding.
Redirects are disabled by default; an explicit bounded allowance supports
301/302/303/307/308 with GET preserved, only within the configured origin. Redirect
userinfo, fragments, cross-origin changes and exhausted hop budgets are rejected.
The base URL cannot contain userinfo, a query or a fragment, and its path is always
treated as a directory. Successful responses must use HTTP 200, the expected media
type and `Xeto-Version: 5`; unsolicited partial or conditional responses fail.

`RepositoryLimits` defines the actual defaults and hard ceilings for response
bytes, JSON nodes and nesting, records, dependency entries, strings, redirects and
total operation time.
Zero or out-of-range budgets fail at construction. Library names have a fixed
128-byte bound, version strings a 62-byte bound, dependency selectors a 256-byte
bound, and each JSON object a 128-member bound. Input bytes are bounded before JSON
parsing; node/container/string limits are checked as values are retained, and
duplicate members and invalid required fields are rejected. Unknown
server fields are bounded; ping preserves them, other DTOs ignore them. Nullable
optional fields preserve unknown values. Search's `more` is the Jeto Marker string
`"✓"`, and publication time uses the native contextual DateTime grammar. Declared
Int fields reuse exact core Jeto conversion: whole decimal/exponent numeric tokens
and integer strings are accepted without binary64 rounding, then their domain
bounds are checked. Fractions, overflow and decimal/exponent integer strings fail.

Each operation has one absolute deadline across all redirect hops and streaming.
Cancellation is checked before dispatch, between response chunks and around
bounded parsing/verification. These checks do not preempt synchronous parser CPU
work. Limits are per operation; callers own aggregate concurrency. Dropping or
cancelling the caller's future stops collection and releases partial bytes. The
client spawns no operation workers, writes no files, and returns no artifact on
partial-body, limit, timeout, cancellation or integrity failure.

Errors distinguish transport, deadline, cancellation, protocol/media type,
metadata, integrity, redirect, generic HTTP and structured API failures. Typed
`UnknownLibErr`, `UnknownLibVersionErr`, `UnknownFuncErr` and `NotImplementedErr`
come from a validated JSON error's `spec`, never from HTTP status alone. Structured
errors preserve the actual HTTP status and the advisory body status separately.
Peer display text, stack traces, response bodies and URLs are omitted from error
diagnostics. A non-JSON 404 is not treated as an unknown library.

Downloads are inert. Neither MIME nor matching digest validates ZIP entries,
archive paths, expanded sizes, source/package consistency or executable semantics.
This API does not extract archives, install dependencies, invoke remote loading,
admit catalogs, activate schemas, or publish repository content. The integration
fixture proves that downloading leaves a managed catalog unchanged, then extracts
one independently pinned stored test entry and separately calls `Catalog::with_project`
and trusted application activation. That proves separation of ownership, not a
general ZIP importer or interoperability with a live upstream repository service.

The wire contracts are pinned to [Xeto 873b9224](https://github.com/Project-Haystack/xeto/tree/873b922451d3ef4c0c9c08ef3daa542f352d69f3/src/xeto/sys.repo),
its [HTTP API](https://github.com/Project-Haystack/xeto/blob/873b922451d3ef4c0c9c08ef3daa542f352d69f3/src/xeto/doc.xeto/HttpApi.md),
and its [library version grammar](https://github.com/Project-Haystack/xeto/blob/873b922451d3ef4c0c9c08ef3daa542f352d69f3/src/xeto/sys/libmeta.xeto).
The exact-byte digest representation is corroborated by
[Haxall aded2799](https://github.com/haxall/haxall/blob/aded27993c2d4834eca4b44ca55671417a1a7ea2/src/core/xetom/fan/util/XetoCrypto.fan).

For optional local serving and the separate bounded admission/activation path,
see [Read-only repositories and explicit package admission](repository-serving-admission.md).
Shared library/version/dependency/digest values are defined in core and re-exported
here; observed HTTP provenance remains owned by this client. Download does not
implicitly call the package validator or application activation APIs.
