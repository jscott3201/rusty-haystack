//! Complete explicit additive closure, prepared privately before publication.
use super::*;
use crate::xeto::catalog::Catalog;
use std::collections::BTreeSet;

/// A failed pure preparation or an embedding's cooperative control stop.
#[derive(Debug, thiserror::Error)]
pub enum PackageAdmissionError<E> {
    #[error(transparent)]
    Package(#[from] PackageError),
    #[error("package preparation stopped: {0}")]
    Control(E),
}
/// Explicit roots, already structurally verified archives and caller-owned marker
/// associations. At most 16 supplied packages and roots, 16 dependency levels,
/// 128 marker bindings, 1 MiB aggregate source and 16 MiB archives. The candidate
/// has at most 128 project declarations and 2048 project slots, including retained
/// project libraries. No resolution,
/// upgrade, filesystem access or network request is implicit.
#[derive(Clone, Debug)]
pub struct PackageSelection {
    roots: Vec<LibraryPin>,
    packages: BTreeMap<String, LibraryPackage>,
    markers: Vec<(String, String)>,
}
#[derive(Default)]
struct DependencyWalk {
    visiting: BTreeSet<String>,
    complete: BTreeMap<String, usize>,
    // Only supplied packages belong in the compile order/reachability count.
    order: Vec<String>,
}
impl PackageSelection {
    pub fn new(
        roots: Vec<LibraryPin>,
        packages: Vec<LibraryPackage>,
        markers: Vec<(String, String)>,
    ) -> Result<Self, PackageError> {
        if roots.is_empty()
            || roots.len() > 16
            || packages.is_empty()
            || packages.len() > 16
            || markers.len() > 128
        {
            return Err(PackageError::Limit("selected packages, roots or markers"));
        }
        let mut selected: BTreeMap<String, LibraryPackage> = BTreeMap::new();
        let mut archive_bytes = 0usize;
        let mut source_bytes = 0usize;
        let mut declarations = 0usize;
        let mut slots = 0usize;
        for package in packages {
            declarations += package.declaration_count;
            slots += package.slot_count;
            if declarations > 128 || slots > 2048 {
                return Err(PackageError::Limit(
                    "aggregate project declarations or slots",
                ));
            }
            archive_bytes = archive_bytes.saturating_add(package.bytes.len());
            source_bytes = source_bytes.saturating_add(package.source.len());
            if archive_bytes > 16 * 1024 * 1024 || source_bytes > 1024 * 1024 {
                return Err(PackageError::Limit("aggregate package bytes"));
            }
            if selected
                .insert(package.pin().lib().into(), package)
                .is_some()
            {
                return Err(PackageError::Conflict);
            }
        }
        let mut names = BTreeSet::new();
        for pin in &roots {
            if !names.insert(pin.lib()) {
                return Err(PackageError::Conflict);
            }
            if selected.get(pin.lib()).is_none_or(|p| p.pin() != pin) {
                return Err(PackageError::MissingDependency);
            }
        }
        let mut marker_names = BTreeSet::new();
        for (marker, qname) in &markers {
            if marker.len() > 128 || qname.len() > 512 || !marker_names.insert(marker) {
                return Err(PackageError::Conflict);
            }
            let Some((lib, _)) = qname.split_once("::") else {
                return Err(PackageError::Invalid("marker target"));
            };
            if !selected.contains_key(lib) {
                return Err(PackageError::Invalid("unselected marker target"));
            }
        }
        Ok(Self {
            roots,
            packages: selected,
            markers,
        })
    }
    /// Pure preparation with no publication. The original catalog is unchanged.
    pub fn admit(&self, base: &Catalog) -> Result<Catalog, PackageError> {
        match self.admit_with(base, || Ok::<_, std::convert::Infallible>(())) {
            Ok(catalog) => Ok(catalog),
            Err(PackageAdmissionError::Package(error)) => Err(error),
            Err(PackageAdmissionError::Control(never)) => match never {},
        }
    }
    /// Cooperatively check embedding control before graph work and between each
    /// bounded source compilation. Individual compiles are bounded, not preempted.
    pub fn admit_with<E>(
        &self,
        base: &Catalog,
        mut checkpoint: impl FnMut() -> Result<(), E>,
    ) -> Result<Catalog, PackageAdmissionError<E>> {
        checkpoint().map_err(PackageAdmissionError::Control)?;
        let mut walk = DependencyWalk::default();
        for root in &self.roots {
            self.visit(root, base, &mut walk, 1, &mut checkpoint)?;
        }
        if walk.order.len() != self.packages.len() {
            return Err(PackageError::Invalid("unreachable supplied package").into());
        }
        // Bound semantic expansion across retained and incoming project sources,
        // before cloning/compiling the candidate. Repeated admissions cannot
        // evade the aggregate bound by choosing smaller batches.
        fn slots_count(slots: &[crate::xeto::Slot]) -> usize {
            slots
                .iter()
                .map(|slot| 1 + slots_count(&slot.children))
                .sum()
        }
        let mut declarations = 0usize;
        let mut slots = 0usize;
        for entry in base
            .declarations()
            .filter(|entry| entry.source.role == "project")
        {
            declarations += 1;
            slots += slots_count(&entry.spec.slots);
        }
        for package in self
            .packages
            .values()
            .filter(|package| !base.libraries().any(|lib| lib.name == package.pin().lib()))
        {
            declarations += package.declaration_count;
            slots += package.slot_count;
        }
        if declarations > 128 || slots > 2048 {
            return Err(PackageError::Limit("aggregate project declarations or slots").into());
        }
        let mut candidate = base.clone();
        for name in walk.order {
            checkpoint().map_err(PackageAdmissionError::Control)?;
            let package = &self.packages[&name];
            let markers: Vec<_> = self
                .markers
                .iter()
                .filter(|(_, q)| q.split_once("::").is_some_and(|(lib, _)| lib == name))
                .map(|(m, q)| (m.as_str(), q.as_str()))
                .collect();
            if candidate.libraries().any(|lib| lib.name == name) {
                if candidate.package_identity(&name)
                    != Some(&(package.pin().clone(), package.digest))
                    || markers
                        .iter()
                        .any(|(m, q)| candidate.package_marker(m) != Some(*q))
                {
                    return Err(PackageError::Conflict.into());
                }
            } else {
                candidate = candidate
                    .with_project(&name, &package.source, &markers)
                    .map_err(PackageError::Catalog)?;
                candidate.record_package(
                    package.pin().clone(),
                    package.digest,
                    package.metadata.maturity,
                );
            }
        }
        checkpoint().map_err(PackageAdmissionError::Control)?;
        Ok(candidate)
    }
    fn visit<E>(
        &self,
        pin: &LibraryPin,
        base: &Catalog,
        walk: &mut DependencyWalk,
        depth: usize,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<usize, PackageAdmissionError<E>> {
        checkpoint().map_err(PackageAdmissionError::Control)?;
        if depth > 16 {
            return Err(PackageError::Limit("dependency depth").into());
        }
        let supplied = self.packages.get(pin.lib());
        if let Some(package) = supplied {
            if package.pin() != pin {
                return Err(PackageError::Conflict.into());
            }
            // Existing identity cannot be inferred from source-only provenance.
            if base.library_pragma(pin.lib()).is_some()
                && base.package_identity(pin.lib()) != Some(&(pin.clone(), package.digest))
            {
                return Err(PackageError::Conflict.into());
            }
        } else {
            let pragma = base
                .library_pragma(pin.lib())
                .ok_or(PackageError::MissingDependency)?;
            if metadata::canonical(&pragma.version)? != pin.version() {
                return Err(PackageError::MissingDependency.into());
            }
        }
        // Every incoming edge must establish its exact pin even on a cache hit.
        if let Some(height) = walk.complete.get(pin.lib()) {
            if depth + height - 1 > 16 {
                return Err(PackageError::Limit("dependency depth").into());
            }
            return Ok(*height);
        }
        if walk.visiting.contains(pin.lib()) {
            return Err(PackageError::Cycle.into());
        }
        // At most 128 admitted project declarations (hence libraries) plus the
        // six supported pinned libraries. Bound traversal before any compilation,
        // including catalogs assembled by the separate source-only API.
        if walk.visiting.len() + walk.complete.len() >= 134 {
            return Err(PackageError::Limit("dependency closure libraries").into());
        }
        walk.visiting.insert(pin.lib().into());
        let retained_dependencies;
        let dependencies = if let Some(package) = supplied {
            &package.dependencies
        } else {
            retained_dependencies = metadata::dependencies(
                base.library_pragma(pin.lib())
                    .ok_or(PackageError::MissingDependency)?,
            )?;
            &retained_dependencies
        };
        let mut height = 1;
        for dependency in dependencies {
            height = height.max(1 + self.visit(dependency, base, walk, depth + 1, checkpoint)?);
        }
        walk.visiting.remove(pin.lib());
        walk.complete.insert(pin.lib().into(), height);
        if supplied.is_some() {
            walk.order.push(pin.lib().into());
        }
        Ok(height)
    }
}
