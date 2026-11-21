//! Immutable read-only repository content and explicit bounded source admission.
mod admission;
mod archive;
mod metadata;
mod types;
pub use admission::{PackageAdmissionError, PackageSelection};
use std::{collections::BTreeMap, sync::Arc};
pub use types::*;

/// Invalid neutral repository value; no transport or admission authority.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RepositoryValueError {
    #[error("invalid repository value: {0}")]
    InvalidInput(&'static str),
    #[error("invalid SHA-256 repository digest")]
    InvalidDigest,
}
/// Rejected package. Errors do not retain untrusted source or archive contents.
#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    #[error(transparent)]
    Value(#[from] RepositoryValueError),
    #[error("invalid package: {0}")]
    Invalid(&'static str),
    #[error("unsupported package profile: {0}")]
    Unsupported(&'static str),
    #[error("package limit exceeded: {0}")]
    Limit(&'static str),
    #[error("conflicting package identity")]
    Conflict,
    #[error("missing selected package dependency")]
    MissingDependency,
    #[error("cyclic package dependency")]
    Cycle,
    #[error("package source is outside the admitted catalog profile")]
    Catalog(#[source] super::catalog::ProfileError),
}
/// Structurally validated source package; not yet semantically admitted.
#[derive(Clone, Debug)]
pub struct LibraryPackage {
    bytes: Arc<[u8]>,
    source: Arc<str>,
    metadata: RepoLibVersion,
    dependencies: Vec<LibraryPin>,
    digest: Sha256Digest,
    declaration_count: usize,
    slot_count: usize,
}
impl LibraryPackage {
    /// Validate a bounded two-file archive and metadata against the requested pin.
    /// Successful construction does not compile or activate source declarations.
    pub fn from_bytes(pin: LibraryPin, bytes: Vec<u8>) -> Result<Self, PackageError> {
        let (properties, source) = archive::unpack(&bytes)?;
        let digest = Sha256Digest::compute(&bytes);
        let (metadata, dependencies, declaration_count, slot_count) =
            metadata::validate(&pin, &properties, &source, digest)?;
        Ok(Self {
            bytes: bytes.into(),
            source: source.into(),
            metadata,
            dependencies,
            digest,
            declaration_count,
            slot_count,
        })
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn metadata(&self) -> &RepoLibVersion {
        &self.metadata
    }
    pub fn pin(&self) -> &LibraryPin {
        &self.metadata.pin
    }
    pub fn digest(&self) -> Sha256Digest {
        self.digest
    }
    pub fn dependencies(&self) -> &[LibraryPin] {
        &self.dependencies
    }
}
/// Finite immutable all-available repository. Configuration is the only input;
/// there is no mutation, filesystem, network or publication API.
#[derive(Clone, Debug)]
pub struct LibraryRepository {
    display: String,
    packages: BTreeMap<LibraryPin, LibraryPackage>,
}
impl LibraryRepository {
    pub fn new(display: &str, packages: Vec<LibraryPackage>) -> Result<Self, PackageError> {
        if display.is_empty() || display.len() > 1024 || packages.len() > 64 {
            return Err(PackageError::Limit("repository configuration"));
        }
        let mut entries: BTreeMap<LibraryPin, LibraryPackage> = BTreeMap::new();
        let mut total = 0usize;
        for package in packages {
            total = total.saturating_add(package.bytes.len());
            if total > 16 * 1024 * 1024 {
                return Err(PackageError::Limit("repository archive bytes"));
            }
            if let Some(prior) = entries.get(package.pin()) {
                if prior.bytes != package.bytes {
                    return Err(PackageError::Conflict);
                }
            } else {
                entries.insert(package.pin().clone(), package);
            }
        }
        Ok(Self {
            display: display.into(),
            packages: entries,
        })
    }
    pub fn display_name(&self) -> &str {
        &self.display
    }
    pub fn packages(&self) -> impl DoubleEndedIterator<Item = &LibraryPackage> {
        self.packages.values()
    }
    pub fn get(&self, pin: &LibraryPin) -> Option<&LibraryPackage> {
        self.packages.get(pin)
    }
}
