//! Read-only Xeto 5 repository client, independent of H4 grids and catalog admission.
//!
//! The four qualified `sys.repo` functions return contextual JSON metadata or exact
//! `application/xetolib` bytes. Downloads remain inert: callers own subsequent
//! archive validation, admission and activation. See `docs/repository-client.md`.
//!
//! ```rust,no_run
//! use haystack_client::repository::{LibraryPin, RepositoryClient, RepositoryError};
//! use tokio_util::sync::CancellationToken;
//!
//! # async fn example() -> Result<(), RepositoryError> {
//! let repository = RepositoryClient::new("https://example.com/xeto/")?;
//! let cancel = CancellationToken::new();
//! let pin = LibraryPin::new("example.equipment", "1.2.0".parse()?)?;
//! let versions = repository.versions(pin.lib(), None, None, &cancel).await?;
//! let advertised = versions.iter().find(|version| version.pin == pin);
//! let download = repository.fetch(&pin, advertised.map(Into::into), None, &cancel).await?;
//! assert_eq!(download.pin, pin);
//! # Ok(())
//! # }
//! ```
mod types;
mod wire;
pub use types::*;
pub use wire::{RepoLibSummary, RepoPing, RepoSearch};

use reqwest::header::{ACCEPT, ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_TYPE, LOCATION};
use sha2::{Digest, Sha256};
use std::{future::Future, net::IpAddr, time::Duration};
use tokio::time::{Instant, timeout_at};
use tokio_util::sync::CancellationToken;
use url::Url;

/// Stable errors never retain peer display text, traces, bodies or URLs.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RepositoryError {
    #[error("invalid repository configuration: {0}")]
    InvalidConfig(&'static str),
    #[error("invalid repository input: {0}")]
    InvalidInput(&'static str),
    #[error("repository transport failed")]
    Transport,
    #[error("repository operation timed out")]
    Timeout,
    #[error("repository operation cancelled")]
    Cancelled,
    #[error("repository response exceeded {0} limit")]
    Limit(&'static str),
    #[error("repository response uses unsupported Xeto version")]
    ProtocolVersion,
    #[error("repository response has unexpected media type")]
    MediaType,
    #[error("repository response uses content encoding")]
    ContentEncoding,
    #[error("malformed repository metadata: {0}")]
    Metadata(&'static str),
    #[error("repository metadata identity differs from request")]
    Identity,
    #[error("invalid SHA-256 repository digest")]
    InvalidDigest,
    #[error("repository artifact digest mismatch")]
    DigestMismatch,
    #[error("repository redirect refused")]
    Redirect,
    #[error("repository returned HTTP {status}")]
    Http { status: u16 },
    #[error("repository API error {kind:?} (HTTP {status})")]
    Api {
        status: u16,
        advisory_status: Option<u16>,
        kind: RepoErrorKind,
    },
}
impl From<haystack_core::xeto::repository::RepositoryValueError> for RepositoryError {
    fn from(error: haystack_core::xeto::repository::RepositoryValueError) -> Self {
        use haystack_core::xeto::repository::RepositoryValueError;
        match error {
            RepositoryValueError::InvalidInput(message) => Self::InvalidInput(message),
            RepositoryValueError::InvalidDigest => Self::InvalidDigest,
        }
    }
}
/// Typed API errors are recognized only from structured error metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepoErrorKind {
    UnknownLibrary,
    UnknownVersion,
    Unsupported,
    Other,
}

/// Bounds apply per logical operation, including all redirects and body reads.
#[derive(Clone, Debug)]
pub struct RepositoryLimits {
    pub timeout: Duration,
    pub metadata_bytes: usize,
    pub metadata_nodes: usize,
    pub metadata_depth: usize,
    pub download_bytes: usize,
    pub records: usize,
    pub dependencies: usize,
    pub string_bytes: usize,
    pub redirects: usize,
}
impl Default for RepositoryLimits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            metadata_bytes: 1024 * 1024,
            metadata_nodes: 32768,
            metadata_depth: 32,
            download_bytes: 16 * 1024 * 1024,
            records: 1024,
            dependencies: 128,
            string_bytes: 4096,
            redirects: 0,
        }
    }
}
impl RepositoryLimits {
    fn validate(&self) -> Result<(), RepositoryError> {
        if self.timeout.is_zero()
            || self.timeout > Duration::from_secs(300)
            || self.metadata_bytes == 0
            || self.metadata_bytes > 16 * 1024 * 1024
            || self.metadata_nodes == 0
            || self.metadata_nodes > 262144
            || self.metadata_depth == 0
            || self.metadata_depth > 64
            || self.download_bytes == 0
            || self.download_bytes > 256 * 1024 * 1024
            || self.records == 0
            || self.records > 16384
            || self.dependencies == 0
            || self.dependencies > 1024
            || self.string_bytes == 0
            || self.string_bytes > 65536
            || self.redirects > 8
        {
            return Err(RepositoryError::InvalidConfig(
                "limits outside supported range",
            ));
        }
        Ok(())
    }
}
/// First-party transport policy; no implicit credentials, proxies, cookies or retries.
#[derive(Clone, Debug, Default)]
pub struct RepositoryConfig {
    pub limits: RepositoryLimits,
    /// Opt in to plaintext only for literal loopback IPs, for deterministic fixtures.
    pub allow_loopback_http: bool,
    /// Optional additional server CA trust. TLS hostname verification stays enabled.
    pub ca_cert_pem: Option<Vec<u8>>,
}

/// Cloneable read-only client. All operation work belongs to the caller's future;
/// cancelling or dropping it stops collection, with no spawned operation workers.
#[derive(Clone)]
pub struct RepositoryClient {
    base: Url,
    http: reqwest::Client,
    limits: RepositoryLimits,
}
impl RepositoryClient {
    /// Create an HTTPS client with default limits and no credentials.
    pub fn new(base: &str) -> Result<Self, RepositoryError> {
        Self::with_config(base, RepositoryConfig::default())
    }
    /// Validate all options before any request. The base path is always a directory.
    pub fn with_config(base: &str, config: RepositoryConfig) -> Result<Self, RepositoryError> {
        config.limits.validate()?;
        let mut base = crate::config::validate_http_url(base)
            .map_err(|_| RepositoryError::InvalidConfig("base URL"))?;
        let loopback = base.host_str().is_some_and(|host| {
            host.trim_matches(['[', ']'])
                .parse::<IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
        });
        if base.scheme() != "https" && !(config.allow_loopback_http && loopback) {
            return Err(RepositoryError::InvalidConfig("HTTPS required"));
        }
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        crate::ensure_crypto_provider();
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .timeout(config.limits.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd();
        if let Some(pem) = config.ca_cert_pem {
            builder = crate::tls::TlsConfig::with_ca(pem)
                .apply(builder)
                .map_err(|_| RepositoryError::InvalidConfig("CA certificate"))?;
        }
        let http = builder
            .build()
            .map_err(|_| RepositoryError::InvalidConfig("HTTP client"))?;
        Ok(Self {
            base,
            http,
            limits: config.limits,
        })
    }
    /// Repository identity/metadata using `sys.repo::repoPing`.
    pub async fn ping(&self, cancel: &CancellationToken) -> Result<RepoPing, RepositoryError> {
        self.operation(cancel, async |deadline| {
            let response = self
                .request("repoPing", &[], false, deadline, cancel)
                .await?;
            let value = wire::ping(&response.bytes, &self.limits)?;
            Ok(value)
        })
        .await
    }
    /// Search results retain server relevance order. `*` means all libraries.
    pub async fn search(
        &self,
        query: &str,
        limit: Option<usize>,
        cancel: &CancellationToken,
    ) -> Result<RepoSearch, RepositoryError> {
        self.text_input(query)?;
        self.check_limit(limit)?;
        let mut params = vec![("query", query.to_owned())];
        if let Some(limit) = limit {
            params.push(("limit", limit.to_string()));
        }
        self.operation(cancel, async |deadline| {
            let response = self
                .request("repoSearch", &params, false, deadline, cancel)
                .await?;
            wire::search(&response.bytes, &self.limits, limit)
        })
        .await
    }
    /// Newest-first numeric versions with their observed source; an unknown
    /// library has an empty list. Mirror fetches retain this source independently.
    pub async fn versions(
        &self,
        lib: &str,
        versions: Option<&DependencyVersions>,
        limit: Option<usize>,
        cancel: &CancellationToken,
    ) -> Result<Vec<RepoAdvertisement>, RepositoryError> {
        types::validate_lib(lib)?;
        self.check_limit(limit)?;
        let mut params = vec![("lib", lib.to_owned())];
        if let Some(versions) = versions {
            params.push(("versions", versions.as_str().to_owned()));
        }
        if let Some(limit) = limit {
            params.push(("limit", limit.to_string()));
        }
        self.operation(cancel, async |deadline| {
            let response = self
                .request("repoVersions", &params, false, deadline, cancel)
                .await?;
            let records = wire::versions(&response.bytes, &self.limits, lib, versions, limit)?;
            let source = std::sync::Arc::new(response.source);
            Ok(records
                .into_iter()
                .map(|metadata| RepoAdvertisement {
                    metadata,
                    source: source.clone(),
                })
                .collect())
        })
        .await
    }
    /// Download one exact pin. Optional observed or supplied metadata must identify the same pin;
    /// every supplied digest is verified. No implicit lookup, upgrade or installation.
    pub async fn fetch(
        &self,
        pin: &LibraryPin,
        metadata: Option<MetadataEvidence<'_>>,
        expected: Option<Sha256Digest>,
        cancel: &CancellationToken,
    ) -> Result<DownloadedLibrary, RepositoryError> {
        let advertised = metadata.map(MetadataEvidence::metadata);
        let metadata_source = metadata.and_then(MetadataEvidence::source);
        if advertised.is_some_and(|m| &m.pin != pin) {
            return Err(RepositoryError::Identity);
        }
        if let Some(metadata) = advertised {
            wire::validate_record(metadata, &self.limits)?;
        }
        let params = [
            ("lib", pin.lib().to_owned()),
            ("version", pin.version().to_string()),
        ];
        self.operation(cancel, async |deadline| {
            let response = self
                .request("repoFetch", &params, true, deadline, cancel)
                .await?;
            check(deadline, cancel)?;
            let computed_digest = response
                .digest
                .ok_or(RepositoryError::Metadata("missing archive digest"))?;
            check(deadline, cancel)?;
            let remote = advertised.and_then(|m| m.digest);
            if remote.is_some_and(|d| d != computed_digest)
                || expected.is_some_and(|d| d != computed_digest)
            {
                return Err(RepositoryError::DigestMismatch);
            }
            let integrity = match (
                remote.is_some(),
                metadata_source.is_some(),
                expected.is_some(),
            ) {
                (false, _, false) => Integrity::Unverified,
                (false, _, true) => Integrity::Caller,
                (true, true, false) => Integrity::Advertised,
                (true, true, true) => Integrity::AdvertisedAndCaller,
                (true, false, false) => Integrity::SuppliedMetadata,
                (true, false, true) => Integrity::SuppliedMetadataAndCaller,
            };
            Ok(DownloadedLibrary {
                pin: pin.clone(),
                bytes: response.bytes,
                metadata: advertised.cloned(),
                metadata_source: metadata_source.cloned(),
                computed_digest,
                caller_digest: expected,
                integrity,
                source: response.source,
            })
        })
        .await
    }
    fn text_input(&self, text: &str) -> Result<(), RepositoryError> {
        if text.is_empty() || text.len() > self.limits.string_bytes {
            Err(RepositoryError::InvalidInput("query length"))
        } else {
            Ok(())
        }
    }
    fn check_limit(&self, limit: Option<usize>) -> Result<(), RepositoryError> {
        if limit.is_some_and(|v| v == 0 || v > self.limits.records) {
            Err(RepositoryError::InvalidInput("record limit"))
        } else {
            Ok(())
        }
    }
    async fn operation<T, F, Fut>(
        &self,
        cancel: &CancellationToken,
        body: F,
    ) -> Result<T, RepositoryError>
    where
        F: FnOnce(Instant) -> Fut,
        Fut: Future<Output = Result<T, RepositoryError>>,
    {
        let deadline = Instant::now()
            .checked_add(self.limits.timeout)
            .ok_or(RepositoryError::InvalidConfig("deadline overflow"))?;
        check(deadline, cancel)?;
        let result = tokio::select! { biased;
            _ = cancel.cancelled() => Err(RepositoryError::Cancelled),
            result = timeout_at(deadline, body(deadline)) => result.map_err(|_| RepositoryError::Timeout)?,
        };
        check(deadline, cancel)?;
        result
    }
    async fn request(
        &self,
        operation: &str,
        params: &[(&str, String)],
        archive: bool,
        deadline: Instant,
        cancel: &CancellationToken,
    ) -> Result<Response, RepositoryError> {
        let mut url = self
            .base
            .join(&format!("./sys.repo::{operation}"))
            .map_err(|_| RepositoryError::InvalidConfig("operation URL"))?;
        if !params.is_empty() {
            url.query_pairs_mut()
                .extend_pairs(params.iter().map(|(k, v)| (*k, v.as_str())));
        }
        let request_url = url.clone();
        let mut redirects = 0;
        loop {
            check(deadline, cancel)?;
            let mut response = self
                .http
                .get(url.clone())
                .header("Xeto-Version", "5")
                .header(
                    ACCEPT,
                    if archive {
                        "application/xetolib"
                    } else {
                        "application/json;box=none"
                    },
                )
                .header(ACCEPT_ENCODING, "identity")
                .send()
                .await
                .map_err(transport_error)?;
            let status = response.status();
            if status.is_redirection() && matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308) {
                if redirects >= self.limits.redirects {
                    return Err(RepositoryError::Redirect);
                }
                let location = one_header(response.headers(), LOCATION.as_str())
                    .ok_or(RepositoryError::Redirect)?;
                if location.len() > self.limits.string_bytes || has_userinfo(location) {
                    return Err(RepositoryError::Redirect);
                }
                let next = url.join(location).map_err(|_| RepositoryError::Redirect)?;
                if next.origin() != self.base.origin()
                    || !next.username().is_empty()
                    || next.password().is_some()
                    || next.fragment().is_some()
                {
                    return Err(RepositoryError::Redirect);
                }
                redirects += 1;
                url = next;
                continue;
            }
            if one_header(response.headers(), "xeto-version") != Some("5") {
                return Err(RepositoryError::ProtocolVersion);
            }
            if response.headers().contains_key(CONTENT_ENCODING)
                && one_header(response.headers(), CONTENT_ENCODING.as_str()) != Some("identity")
            {
                return Err(RepositoryError::ContentEncoding);
            }
            let media = one_header(response.headers(), CONTENT_TYPE.as_str())
                .ok_or(RepositoryError::MediaType)?;
            let json = media_is(media, "application/json");
            if status.is_success() && status.as_u16() != 200 {
                return Err(RepositoryError::Http {
                    status: status.as_u16(),
                });
            }
            if status.is_success() {
                if !(if archive {
                    media_is(media, "application/xetolib")
                } else {
                    json
                }) {
                    return Err(RepositoryError::MediaType);
                }
            } else if !json {
                return Err(RepositoryError::Http {
                    status: status.as_u16(),
                });
            }
            let maximum = if archive && status.is_success() {
                self.limits.download_bytes
            } else {
                self.limits.metadata_bytes
            };
            if response
                .content_length()
                .is_some_and(|n| n > maximum as u64)
            {
                return Err(RepositoryError::Limit("body bytes"));
            }
            let mut bytes = Vec::new();
            let mut hash = (archive && status.is_success()).then(Sha256::new);
            while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
                check(deadline, cancel)?;
                if chunk.len() > maximum.saturating_sub(bytes.len()) {
                    return Err(RepositoryError::Limit("body bytes"));
                }
                if let Some(hash) = &mut hash {
                    hash.update(&chunk);
                }
                bytes.extend_from_slice(&chunk);
                check(deadline, cancel)?;
            }
            check(deadline, cancel)?;
            if !status.is_success() {
                return Err(wire::api_error(status.as_u16(), &bytes, &self.limits));
            }
            return Ok(Response {
                bytes,
                digest: hash.map(|hash| Sha256Digest::from_bytes(hash.finalize().into())),
                source: RepositorySource {
                    request_url,
                    final_url: url,
                    redirects,
                },
            });
        }
    }
}
struct Response {
    bytes: Vec<u8>,
    digest: Option<Sha256Digest>,
    source: RepositorySource,
}
fn check(deadline: Instant, cancel: &CancellationToken) -> Result<(), RepositoryError> {
    if cancel.is_cancelled() {
        Err(RepositoryError::Cancelled)
    } else if Instant::now() >= deadline {
        Err(RepositoryError::Timeout)
    } else {
        Ok(())
    }
}
fn transport_error(error: reqwest::Error) -> RepositoryError {
    if error.is_timeout() {
        RepositoryError::Timeout
    } else {
        RepositoryError::Transport
    }
}
fn one_header<'a>(headers: &'a reqwest::header::HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let first = values.next()?.to_str().ok()?;
    if values.next().is_some() {
        None
    } else {
        Some(first)
    }
}
fn media_is(value: &str, expected: &str) -> bool {
    value
        .split(';')
        .next()
        .is_some_and(|v| v.trim().eq_ignore_ascii_case(expected))
}
fn has_userinfo(location: &str) -> bool {
    let authority = location
        .split_once("://")
        .map(|(_, tail)| tail)
        .or_else(|| location.strip_prefix("//"));
    authority.is_some_and(|a| {
        a.split(['/', '?', '#'])
            .next()
            .is_some_and(|a| a.contains('@'))
    })
}
