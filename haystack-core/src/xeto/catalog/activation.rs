//! One immutable observation for query lookup, native fitting, and call codecs.
//!
//! An [`ActivatedCatalog`] is the only unit a managed graph publishes: its strict
//! selection, the compatibility namespace derived from it, declaration
//! provenance and compiled callable codec contexts always change together.
//! Runtime publication generation, the selection digest and the upstream source
//! revision stored in nominal values are three distinct identities.
use super::*;
use crate::graph::EntityGraph;
use crate::ontology::DefNamespace;
use std::sync::Arc;

pub struct ActivatedCatalog {
    catalog: Arc<Catalog>,
    namespace: Arc<DefNamespace>,
    callables: Arc<BTreeMap<String, CallableContext>>,
    selection: Arc<str>,
}

impl std::fmt::Debug for ActivatedCatalog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActivatedCatalog")
            .field("selection", &self.selection)
            .finish_non_exhaustive()
    }
}

impl ActivatedCatalog {
    /// Compile all selected callable contexts before graph publication. The
    /// compatibility namespace is a derived view of this exact selection.
    pub fn new(catalog: Catalog, namespace: Option<&DefNamespace>) -> Result<Self, ProfileError> {
        let mut callables = BTreeMap::new();
        for declaration in catalog.operations() {
            callables.insert(
                declaration.spec.qname.clone(),
                CallableContext::new(&catalog, declaration)?,
            );
        }
        let selection = Arc::from(catalog.selection_identity());
        let catalog = Arc::new(catalog);
        let namespace = Arc::new(
            namespace
                .cloned()
                .unwrap_or_default()
                .with_catalog(catalog.clone()),
        );
        Ok(Self {
            catalog,
            namespace,
            callables: Arc::new(callables),
            selection,
        })
    }

    /// The same selection derived over a different trusted compatibility
    /// namespace. Declarations, provenance and callable contexts are shared.
    pub fn rebase(&self, namespace: DefNamespace) -> Self {
        Self {
            catalog: self.catalog.clone(),
            namespace: Arc::new(namespace.with_catalog(self.catalog.clone())),
            callables: self.callables.clone(),
            selection: self.selection.clone(),
        }
    }

    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }
    pub fn namespace(&self) -> &Arc<DefNamespace> {
        &self.namespace
    }
    pub fn callable(&self, qname: &str) -> Option<&CallableContext> {
        self.callables.get(qname)
    }
    /// Digest of selected declaration sources and explicit associations.
    /// Equal digests do not make an observation current; graph publication
    /// identity does.
    pub fn selection_identity(&self) -> &str {
        &self.selection
    }

    /// Reject a candidate that claims an already admitted upstream origin for
    /// different declaration bytes. Project declarations carry their own digest
    /// and may be replaced by a later selection.
    pub fn check_provenance(&self, previous: &Self) -> Result<(), ProfileError> {
        let (old, new) = (&previous.catalog, &self.catalog);
        if old.provenance.repository != new.provenance.repository
            || old.provenance.commit != new.provenance.commit
        {
            return Err(source_error(
                "catalog",
                "candidate changes the admitted source origin",
            ));
        }
        for (qname, before) in &old.specs {
            if before.source.role == "project" {
                continue;
            }
            if let Some(after) = new.specs.get(qname)
                && (after.source.role == "project"
                    || after.source.path != before.source.path
                    || after.source.sha256 != before.source.sha256
                    || after.spec != before.spec)
            {
                return Err(resolve_error(
                    &after.source,
                    qname,
                    "declaration differs from the admitted bytes at the same origin",
                ));
            }
        }
        Ok(())
    }

    /// Validate every graph record associated with this candidate or with
    /// `previous`, including records an application hides from callers. An
    /// association is an explicit `spec` reference into an admitted library or
    /// an explicit marker binding. Unloading a declaration does not discard its
    /// obligation: the candidate must still admit and fit it. Untyped records
    /// are never required to fit every admitted schema. No value is inserted,
    /// repaired or relabeled.
    pub fn validate_graph<C: ActivationControl>(
        &self,
        previous: Option<&Self>,
        graph: &EntityGraph,
        control: &mut C,
    ) -> Result<(), ActivationError<C::Error>> {
        let mut after = None;
        while let Some(last) =
            self.validate_chunk(previous, graph, after.as_deref(), usize::MAX, control)?
        {
            after = Some(last);
        }
        Ok(())
    }

    /// Validate at most `limit` records in stable id order after `after`, under
    /// the caller's borrow. Returns the last validated id when records remain.
    /// A caller releasing its graph guard between chunks must prove the graph
    /// state is unchanged across all chunks before relying on the result.
    /// Bytes retained while fitting a record are released when it completes.
    pub fn validate_chunk<C: ActivationControl>(
        &self,
        previous: Option<&Self>,
        graph: &EntityGraph,
        after: Option<&str>,
        limit: usize,
        control: &mut C,
    ) -> Result<Option<String>, ActivationError<C::Error>> {
        let limit = limit.max(1);
        let catalogs = [Some(self), previous];
        let libraries: BTreeSet<&str> = catalogs
            .iter()
            .flatten()
            .flat_map(|catalog| catalog.catalog.libraries.keys().map(String::as_str))
            .collect();
        let markers: BTreeSet<(&str, &str)> = catalogs
            .iter()
            .flatten()
            .flat_map(|catalog| catalog.catalog.marker_bindings())
            .collect();
        let mut last = None;
        for (validated, (id, record)) in graph.entities_after(after).enumerate() {
            if validated == limit {
                return Ok(last.map(str::to_owned));
            }
            last = Some(id);
            control.work(1).map_err(ActivationError::Control)?;
            let mut associated = BTreeSet::new();
            if let Some(Kind::Ref(spec)) = record.get("spec") {
                control
                    .work(spec.val.len().saturating_add(1))
                    .map_err(ActivationError::Control)?;
                if spec
                    .val
                    .split_once("::")
                    .is_some_and(|(library, _)| libraries.contains(library))
                {
                    associated.insert(spec.val.as_str());
                }
            }
            for &(marker, qname) in &markers {
                control.work(1).map_err(ActivationError::Control)?;
                if record.has(marker) {
                    associated.insert(qname);
                }
            }
            for qname in associated {
                let reject = |cause| {
                    ActivationError::Rejected(Box::new(ActivationRejection {
                        entity: id.into(),
                        association: qname.into(),
                        cause,
                    }))
                };
                if self.catalog.declaration(qname).is_none() {
                    return Err(reject(ProfileError::Resolve {
                        path: "catalog".into(),
                        declaration: qname.into(),
                        message: "affected association is not admitted by the candidate".into(),
                    }));
                }
                let mut env = GraphFit {
                    graph,
                    control: &mut *control,
                    retained: 0,
                };
                let fitted = self.catalog.fit_entity(qname, record, &mut env);
                let retained = env.retained;
                control.release(retained);
                match fitted {
                    Ok(()) => {}
                    Err(FitError::Invalid(cause)) => return Err(reject(cause)),
                    Err(FitError::Control(error)) => return Err(ActivationError::Control(error)),
                }
            }
        }
        Ok(None)
    }
}

/// Caller-owned limits and authority for one activation. Every method may stop
/// the activation; a stop before [`ActivationControl::publish`] returns `Ok`
/// never publishes.
pub trait ActivationControl {
    type Error;
    fn work(&mut self, amount: usize) -> Result<(), Self::Error>;
    /// Bytes retained while fitting the current record.
    fn retain(&mut self, bytes: usize) -> Result<(), Self::Error>;
    /// Bytes retained for a record whose fitting finished are released.
    fn release(&mut self, bytes: usize);
    fn depth(&mut self, depth: usize) -> Result<(), Self::Error>;
    /// One bounded lock wait within the original deadline.
    fn wait(&mut self) -> Result<std::time::Duration, Self::Error>;
    /// Records validated under one read guard. The guard is released between
    /// chunks; any graph change across chunks restarts validation.
    fn chunk_records(&self) -> usize {
        256
    }
    /// Restarts after entity-only changes before the activation reports
    /// [`ActivationError::Conflict`] instead of retrying indefinitely.
    fn max_revalidations(&self) -> usize {
        3
    }
    /// Application admission of the complete candidate, before graph reads.
    /// An application that cannot execute it returns
    /// [`ActivationError::Unsupported`].
    fn admit(
        &mut self,
        candidate: &Arc<ActivatedCatalog>,
    ) -> Result<(), ActivationError<Self::Error>>;
    /// The commit point. It runs while holding the final write lock, after its
    /// wait and after the graph state and observation identity matched the
    /// validated ones: recheck authority, sealing, deadline and capacity. If
    /// it returns `Ok`, the replacement follows with no further fallible step.
    fn publish(&mut self) -> Result<(), Self::Error>;
}

/// Activation failed and nothing was published.
pub enum ActivationError<E> {
    /// Candidate parse, resolution, provenance or callable compilation failed.
    Catalog(Box<ProfileError>),
    /// The application cannot bind its supported handlers to the candidate.
    Unsupported { declaration: String, reason: String },
    /// Affected graph data does not fit the candidate.
    Rejected(Box<ActivationRejection>),
    /// The graph, its catalog or the retained observation changed, or entity
    /// writes outlasted the bounded revalidations.
    Conflict,
    /// The graph has no managed observation to replace.
    Unmanaged,
    /// Caller limits, cancellation or authority stopped the activation.
    Control(E),
}

impl<E> std::fmt::Debug for ActivationError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Catalog(_) => "Catalog",
            Self::Unsupported { .. } => "Unsupported",
            Self::Rejected(_) => "Rejected",
            Self::Conflict => "Conflict",
            Self::Unmanaged => "Unmanaged",
            Self::Control(_) => "Control",
        })
    }
}

/// Graph-validation failure. `Display` and `Debug` are public-safe: they never
/// contain entity identities, values, references or schema names. Privileged
/// operators read [`ActivationRejection::privileged`] explicitly.
pub struct ActivationRejection {
    entity: String,
    association: String,
    cause: ProfileError,
}
impl ActivationRejection {
    /// Entity id, associated declaration and fitting cause.
    pub fn privileged(&self) -> (&str, &str, &ProfileError) {
        (&self.entity, &self.association, &self.cause)
    }
}
impl std::fmt::Display for ActivationRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("affected graph data does not fit the candidate catalog")
    }
}
impl std::fmt::Debug for ActivationRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ActivationRejection")
    }
}

struct GraphFit<'g, 'c, C> {
    graph: &'g EntityGraph,
    control: &'c mut C,
    retained: usize,
}
impl<'g, C: ActivationControl> FitEnvironment<'g> for GraphFit<'g, '_, C> {
    type Error = C::Error;
    fn work(&mut self, amount: usize) -> Result<(), C::Error> {
        self.control.work(amount)
    }
    fn retain(&mut self, bytes: usize) -> Result<(), C::Error> {
        self.control.retain(bytes)?;
        self.retained = self.retained.saturating_add(bytes);
        Ok(())
    }
    fn depth(&mut self, depth: usize) -> Result<(), C::Error> {
        self.control.depth(depth)
    }
    fn record(&mut self, id: &HRef) -> Result<Option<FitRecord<'g>>, C::Error> {
        Ok(self.graph.get(&id.val).map(FitRecord::Borrowed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn altered_bytes_under_the_admitted_origin_are_rejected() {
        let old = ActivatedCatalog::new(Catalog::load_http_pinned().unwrap(), None).unwrap();
        let mut altered = Catalog::load_http_pinned().unwrap();
        let entry = altered.specs.get_mut("sys::Dict").unwrap();
        entry.source.sha256 = "0".repeat(64);
        let candidate = ActivatedCatalog::new(altered, None).unwrap();
        assert!(candidate.check_provenance(&old).is_err());

        let mut altered = Catalog::load_http_pinned().unwrap();
        altered
            .specs
            .get_mut("sys::Str")
            .unwrap()
            .spec
            .doc
            .push('!');
        let candidate = ActivatedCatalog::new(altered, None).unwrap();
        assert!(candidate.check_provenance(&old).is_err());

        let mut foreign = Catalog::load_http_pinned().unwrap();
        foreign.provenance.commit = "0".repeat(40);
        let candidate = ActivatedCatalog::new(foreign, None).unwrap();
        assert!(candidate.check_provenance(&old).is_err());

        let additive =
            ActivatedCatalog::new(Catalog::load_protocol_pinned().unwrap(), None).unwrap();
        additive.check_provenance(&old).unwrap();
    }

    #[test]
    fn rejection_diagnostics_do_not_disclose_hidden_identity() {
        let rejection = ActivationRejection {
            entity: "hidden-entity".into(),
            association: "secret::Schema".into(),
            cause: ProfileError::Fit {
                path: "catalog".into(),
                slot: "secret::Schema.value".into(),
                expected: "sys::Str".into(),
                message: "missing required slot".into(),
            },
        };
        for public in [
            rejection.to_string(),
            format!("{rejection:?}"),
            format!("{:?}", ActivationError::<()>::Rejected(Box::new(rejection))),
        ] {
            assert!(!public.contains("hidden"), "{public}");
            assert!(!public.contains("secret"), "{public}");
        }
    }
}
