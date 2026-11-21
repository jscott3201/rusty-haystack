//! Trusted preparation retains its original active observation through publication.
use super::*;
use haystack_core::xeto::{
    catalog::{ActivatedCatalog, ActivationError, Catalog},
    repository::{PackageAdmissionError, PackageSelection},
};

pub type ApplicationPackageError = PackageAdmissionError<ReadError>;
/// Preparation owns a separate deadline from a later activation call. Input and
/// aggregate expansion/closure limits are fixed by core's selected profile.
#[derive(Clone, Copy, Debug)]
pub struct PackagePreparationLimits {
    pub max_duration: std::time::Duration,
}
impl Default for PackagePreparationLimits {
    fn default() -> Self {
        Self {
            max_duration: std::time::Duration::from_secs(60),
        }
    }
}
impl PackagePreparationLimits {
    fn validate(self) -> Result<(), ReadError> {
        if self.max_duration.is_zero() || self.max_duration > std::time::Duration::from_secs(60) {
            Err(ReadError::InvalidLimits)
        } else {
            Ok(())
        }
    }
}
/// A complete private candidate bound to the precise publication observed before
/// preparation. Immutable source equality does not substitute for currentness.
/// Activation requires this exact observation to remain current in its graph.
#[derive(Debug)]
pub struct PreparedPackageAdmission {
    expected: Arc<ActivatedCatalog>,
    candidate: Catalog,
}
impl PreparedPackageAdmission {
    pub fn selection_identity(&self) -> String {
        self.candidate.selection_identity()
    }
}
impl ReadService {
    /// Prepare structurally verified package inputs as the trusted embedding.
    /// This does not publish. Archive validation is a separate bounded pure core
    /// operation; semantic compilation runs under this application's worker lease.
    pub async fn prepare_packages(
        &self,
        context: ReadContext,
        selection: PackageSelection,
    ) -> Result<PreparedPackageAdmission, ApplicationPackageError> {
        self.prepare_packages_with(context, selection, PackagePreparationLimits::default())
            .await
    }
    pub async fn prepare_packages_with(
        &self,
        context: ReadContext,
        selection: PackageSelection,
        limits: PackagePreparationLimits,
    ) -> Result<PreparedPackageAdmission, ApplicationPackageError> {
        limits.validate().map_err(PackageAdmissionError::Control)?;
        let guard = self
            .inner
            .lifecycle
            .as_ref()
            .map(|life| life.admit().map_err(application_read_error))
            .transpose()
            .map_err(PackageAdmissionError::Control)?;
        self.begin_with_guard(context, guard, None, limits.max_duration)
            .await
            .map_err(PackageAdmissionError::Control)?
            .prepare_packages(selection)
            .await
    }
    /// Publish the prepared candidate only if its original observation is still
    /// current. Reuses all activation authority, validation and commit controls.
    /// This is a new bounded activation call, not a continuation of prep's deadline.
    pub async fn activate_prepared_packages(
        &self,
        context: ReadContext,
        prepared: PreparedPackageAdmission,
        limits: CatalogActivationLimits,
    ) -> Result<CatalogActivation, CatalogActivationError> {
        limits.validate().map_err(ActivationError::Control)?;
        let guard = self
            .inner
            .lifecycle
            .as_ref()
            .map(|life| life.admit().map_err(application_read_error))
            .transpose()
            .map_err(ActivationError::Control)?;
        self.begin_with_guard(context, guard, None, limits.max_duration)
            .await
            .map_err(ActivationError::Control)?
            .activate_catalog_expected(prepared.candidate, limits, Some(prepared.expected))
            .await
    }
}
impl ReadAdmission {
    /// Preparation under this admission's original deadline and worker lease.
    pub async fn prepare_packages(
        self,
        selection: PackageSelection,
    ) -> Result<PreparedPackageAdmission, ApplicationPackageError> {
        let inner = self.inner.clone();
        self.run_task(move |principal, budget| {
            if !matches!(principal, Principal::TrustedEmbedding { .. }) {
                return Err(ReadError::Forbidden);
            }
            activation::publish_check(budget)?;
            let expected = loop {
                let wait = budget.wait_quantum()?;
                if let Some(current) = inner
                    .graph
                    .read_for(wait, |graph| graph.activated_catalog().cloned())
                {
                    break current;
                }
            }
            .ok_or(ReadError::Unavailable)?;
            #[cfg(test)]
            if let Some(hook) = &budget.package_prepare_hook {
                hook();
            }
            let candidate =
                selection.admit_with(expected.catalog(), || activation::publish_check(budget));
            Ok(candidate.map(|candidate| PreparedPackageAdmission {
                expected,
                candidate,
            }))
        })
        .await
        .map_err(PackageAdmissionError::Control)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AllowAll, ApiError, ApplicationBuilder, ShutdownPolicy, TypedInvocationInput};
    use haystack_core::{
        graph::EntityGraph,
        kinds::HRef,
        xeto::repository::{LibraryPackage, LibraryPin},
    };
    use std::time::Duration;
    fn trusted() -> ReadContext {
        ReadContext::with_timeout(
            Principal::TrustedEmbedding {
                subject: "owner".into(),
            },
            Duration::from_secs(5),
        )
    }
    fn selection() -> PackageSelection {
        let pin = LibraryPin::new("sample", "1.0.0".parse().unwrap()).unwrap();
        let package = LibraryPackage::from_bytes(
            pin.clone(),
            include_bytes!(
                "../../../haystack-core/tests/fixtures/repository/sample-deflate.xetolib"
            )
            .to_vec(),
        )
        .unwrap();
        PackageSelection::new(vec![pin], vec![package], vec![]).unwrap()
    }
    fn setup() -> (SharedGraph, ReadService) {
        let graph = SharedGraph::new(EntityGraph::new());
        let reads =
            ReadService::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default()).unwrap();
        (graph, reads)
    }
    fn observation(graph: &SharedGraph) -> Arc<ActivatedCatalog> {
        graph.read(|g| g.activated_catalog().cloned().unwrap())
    }
    fn pause() -> (
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::Sender<()>,
        Arc<dyn Fn() + Send + Sync>,
    ) {
        let (entered_tx, entered) = std::sync::mpsc::channel();
        let (release, rx) = std::sync::mpsc::channel();
        let rx = std::sync::Mutex::new(rx);
        let hook = Arc::new(move || {
            entered_tx.send(()).unwrap();
            rx.lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        });
        (entered, release, hook)
    }
    async fn entered(rx: std::sync::mpsc::Receiver<()>) {
        tokio::task::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(5)))
            .await
            .unwrap()
            .unwrap();
    }
    async fn drained(reads: &ReadService) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while reads.load().admitted != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn prepared_candidate_rejects_new_and_identical_selection_observations() {
        for identical in [false, true] {
            let (graph, reads) = setup();
            let prepared = reads
                .prepare_packages(trusted(), selection())
                .await
                .unwrap();
            let old = observation(&graph);
            let replacement = if identical {
                old.catalog().clone()
            } else {
                old.catalog().with_project("other","pragma: Lib <version:\"1.0.0\",depends:{{lib:\"sys\",versions:\"5.0.0\"}}>\nOther: Dict {}",&[]).unwrap()
            };
            reads
                .activate_catalog(trusted(), replacement)
                .await
                .unwrap();
            let current = observation(&graph);
            let state = graph.state();
            let mut wakes = graph.subscribe_wakes();
            assert!(!Arc::ptr_eq(&old, &current));
            if identical {
                assert_eq!(old.selection_identity(), current.selection_identity());
            }
            assert!(matches!(
                reads
                    .activate_prepared_packages(
                        trusted(),
                        prepared,
                        CatalogActivationLimits::default()
                    )
                    .await,
                Err(ActivationError::Conflict)
            ));
            assert!(Arc::ptr_eq(&current, &observation(&graph)));
            assert_eq!(state, graph.state());
            assert!(wakes.try_recv().is_err());
        }
    }
    #[tokio::test]
    async fn preparation_authority_limits_and_failed_activation_preserve_catalog() {
        let (graph, reads) = setup();
        let old = observation(&graph);
        let state = graph.state();
        assert!(matches!(
            reads
                .prepare_packages(
                    ReadContext::with_timeout(Principal::Anonymous, Duration::from_secs(1)),
                    selection()
                )
                .await,
            Err(PackageAdmissionError::Control(ReadError::Forbidden))
        ));
        assert!(matches!(
            reads
                .prepare_packages_with(
                    trusted(),
                    selection(),
                    PackagePreparationLimits {
                        max_duration: Duration::ZERO
                    }
                )
                .await,
            Err(PackageAdmissionError::Control(ReadError::InvalidLimits))
        ));
        assert!(Arc::ptr_eq(&old, &observation(&graph)));
        assert_eq!(state, graph.state());
        let mut row = HDict::new();
        row.set("id", Kind::Ref(HRef::from_val("hidden-invalid")));
        row.set("spec", Kind::Ref(HRef::from_val("sample::Thing")));
        graph.add(row).unwrap();
        let prepared = reads
            .prepare_packages(trusted(), selection())
            .await
            .unwrap();
        let state = graph.state();
        let mut wakes = graph.subscribe_wakes();
        let error = reads
            .activate_prepared_packages(trusted(), prepared, CatalogActivationLimits::default())
            .await
            .unwrap_err();
        assert!(matches!(error, ActivationError::Rejected(_)));
        assert!(!format!("{error:?}").contains("hidden-invalid"));
        assert!(Arc::ptr_eq(&old, &observation(&graph)));
        assert_eq!(state, graph.state());
        assert!(wakes.try_recv().is_err());
        let mut correction = HDict::new();
        correction.set("name", Kind::Str("valid after correction".into()));
        graph.update("hidden-invalid", correction).unwrap();
        let prepared = reads
            .prepare_packages(trusted(), selection())
            .await
            .unwrap();
        let mut wakes = graph.subscribe_wakes();
        reads
            .activate_prepared_packages(trusted(), prepared, CatalogActivationLimits::default())
            .await
            .unwrap();
        assert!(!Arc::ptr_eq(&old, &observation(&graph)));
        assert!(wakes.try_recv().is_ok());
        assert!(wakes.try_recv().is_err());
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn package_activation_keeps_running_request_and_encoding_coherent() {
        let (graph, reads) = setup();
        let mut row = HDict::new();
        row.set("id", Kind::Ref(HRef::from_val("entity")));
        row.set("dis", Kind::Str("one revision".into()));
        graph.add(row).unwrap();
        let input = || TypedInvocationInput {
            operation: "readById".into(),
            versions: vec!["5".into()],
            post: true,
            content_types: vec!["application/json".into()],
            body: br#"{"id":"entity"}"#.to_vec(),
            ..Default::default()
        };
        for encoding in [false, true] {
            let prepared = reads
                .prepare_packages(trusted(), selection())
                .await
                .unwrap();
            let (rx, release, hook) = pause();
            let mut admission = reads.begin(trusted()).await.unwrap();
            if encoding {
                admission.budget_mut().typed_encode_hook = Some(hook);
            } else {
                admission.budget_mut().typed_lookup_hook = Some(hook);
            }
            let request = tokio::spawn(admission.invoke_wire(input()));
            entered(rx).await;
            reads
                .activate_prepared_packages(trusted(), prepared, CatalogActivationLimits::default())
                .await
                .unwrap();
            release.send(()).unwrap();
            let result = request.await.unwrap();
            if encoding {
                let old = result.unwrap().body;
                let fresh = reads
                    .begin(trusted())
                    .await
                    .unwrap()
                    .invoke_wire(input())
                    .await
                    .unwrap()
                    .body;
                assert_eq!(old, fresh);
            } else {
                assert!(matches!(result, Err(ApiError::Unavailable)));
            }
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn preparation_cancel_deadline_and_drop_keep_worker_lease_until_exit() {
        for stop in ["cancel", "deadline", "drop"] {
            let (graph, reads) = setup();
            let old = observation(&graph);
            let state = graph.state();
            let (rx, release, hook) = pause();
            let mut admission = reads.begin(trusted()).await.unwrap();
            let cancel = admission.cancellation();
            if stop == "deadline" {
                admission.budget_mut().deadline = Instant::now() + Duration::from_millis(200);
            }
            admission.budget_mut().package_prepare_hook = Some(hook);
            let work = tokio::spawn(admission.prepare_packages(selection()));
            entered(rx).await;
            match stop {
                "cancel" => {
                    cancel.cancel();
                    assert!(matches!(
                        work.await.unwrap(),
                        Err(PackageAdmissionError::Control(ReadError::Cancelled))
                    ));
                }
                "deadline" => {
                    assert!(matches!(
                        work.await.unwrap(),
                        Err(PackageAdmissionError::Control(ReadError::Deadline))
                    ));
                }
                _ => {
                    work.abort();
                    assert!(work.await.unwrap_err().is_cancelled());
                }
            }
            assert_eq!(reads.load().admitted, 1, "blocked worker still owns lease");
            assert!(Arc::ptr_eq(&old, &observation(&graph)));
            assert_eq!(graph.state(), state);
            release.send(()).unwrap();
            drained(&reads).await;
            assert_eq!(reads.load().admitted, 0);
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn owner_close_seals_preparation_and_waits_for_actual_worker() {
        let graph = SharedGraph::new(EntityGraph::new());
        let builder =
            ApplicationBuilder::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default())
                .unwrap()
                .shutdown_policy(ShutdownPolicy {
                    drain_timeout: Duration::from_secs(3),
                    stop_timeout: Duration::from_secs(1),
                });
        let handle = builder.handle();
        let reads = handle.read_service();
        let owner = builder.start(&tokio::runtime::Handle::current()).unwrap();
        owner.ready().await.unwrap();
        let old = observation(&graph);
        let (rx, release, hook) = pause();
        let mut admission = reads.begin(trusted()).await.unwrap();
        admission.budget_mut().package_prepare_hook = Some(hook);
        let work = tokio::spawn(admission.prepare_packages(selection()));
        entered(rx).await;
        let closing = tokio::spawn(async move { owner.close().await });
        handle.closing().cancelled().await;
        assert!(!closing.is_finished());
        assert_eq!(reads.load().admitted, 1);
        release.send(()).unwrap();
        assert!(matches!(
            work.await.unwrap(),
            Err(PackageAdmissionError::Control(ReadError::Cancelled))
        ));
        closing.await.unwrap().unwrap();
        assert!(Arc::ptr_eq(&old, &observation(&graph)));
        assert_eq!(reads.load().admitted, 0);
    }
}
