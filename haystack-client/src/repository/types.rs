//! Client-owned observed provenance, separate from shared repository values.
pub(super) use haystack_core::xeto::repository::validate_lib;
pub use haystack_core::xeto::repository::{
    Availability, DependencyVersions, LibraryPin, LibraryVersion, Maturity, RepoDependency,
    RepoLibVersion, Sha256Digest,
};

/// Metadata observed by this client at a repository endpoint. Source and metadata
/// are immutable together; callers can clone the DTO to make a separate assertion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoAdvertisement {
    pub(super) metadata: RepoLibVersion,
    pub(super) source: std::sync::Arc<RepositorySource>,
}
impl RepoAdvertisement {
    /// The exact decoded advertisement observed at `source()`.
    pub fn metadata(&self) -> &RepoLibVersion {
        &self.metadata
    }
    /// Metadata request/final URL and redirects, independent of a later fetch.
    pub fn source(&self) -> &RepositorySource {
        &self.source
    }
}
impl std::ops::Deref for RepoAdvertisement {
    type Target = RepoLibVersion;
    fn deref(&self) -> &Self::Target {
        &self.metadata
    }
}
/// Verification metadata has either an observed repository source or is an
/// explicitly caller-supplied assertion. Neither form grants admission authority.
#[derive(Clone, Copy, Debug)]
pub enum MetadataEvidence<'a> {
    Observed(&'a RepoAdvertisement),
    Supplied(&'a RepoLibVersion),
}
impl<'a> MetadataEvidence<'a> {
    pub(super) fn metadata(self) -> &'a RepoLibVersion {
        match self {
            Self::Observed(value) => value.metadata(),
            Self::Supplied(value) => value,
        }
    }
    pub(super) fn source(self) -> Option<&'a RepositorySource> {
        match self {
            Self::Observed(value) => Some(value.source()),
            Self::Supplied(_) => None,
        }
    }
}
impl<'a> From<&'a RepoAdvertisement> for MetadataEvidence<'a> {
    fn from(value: &'a RepoAdvertisement) -> Self {
        Self::Observed(value)
    }
}
impl<'a> From<&'a RepoLibVersion> for MetadataEvidence<'a> {
    fn from(value: &'a RepoLibVersion) -> Self {
        Self::Supplied(value)
    }
}

/// Which separately supplied expectations matched the computed exact-byte hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Integrity {
    Unverified,
    Advertised,
    Caller,
    AdvertisedAndCaller,
    /// A digest in unbound caller-supplied metadata matched.
    SuppliedMetadata,
    /// Both an unbound metadata digest and the separate caller digest matched.
    SuppliedMetadataAndCaller,
}
/// Request and final source of a metadata or artifact response. URLs exclude userinfo;
/// redirect targets with userinfo or fragments are rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositorySource {
    pub request_url: url::Url,
    pub final_url: url::Url,
    pub redirects: usize,
}
/// An inert artifact. No extraction, execution, filesystem write or catalog mutation.
#[derive(Clone, Debug)]
pub struct DownloadedLibrary {
    pub pin: LibraryPin,
    pub bytes: Vec<u8>,
    pub metadata: Option<RepoLibVersion>,
    /// Source of an observed advertisement, independent of the artifact source.
    /// `None` means absent metadata or explicitly caller-supplied metadata.
    pub metadata_source: Option<RepositorySource>,
    pub computed_digest: Sha256Digest,
    pub caller_digest: Option<Sha256Digest>,
    pub integrity: Integrity,
    pub source: RepositorySource,
}
