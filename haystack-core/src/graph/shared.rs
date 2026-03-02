// SharedGraph — thread-safe wrapper around EntityGraph using parking_lot RwLock.

use parking_lot::RwLock;
use std::sync::Arc;
use tokio::sync::broadcast;

use crate::data::{HDict, HGrid};
use crate::ontology::ValidationIssue;

use super::entity_graph::{EntityGraph, GraphError, HierarchyNode};

/// Default broadcast channel capacity.
const BROADCAST_CAPACITY: usize = 256;

/// Thread-safe, clonable handle to an `EntityGraph`.
///
/// Uses `parking_lot::RwLock` for reader-writer locking, allowing
/// concurrent reads with exclusive writes. Cloning shares the
/// underlying graph (via `Arc`).
///
/// Write operations automatically send the new graph version on an
/// internal broadcast channel. Call [`subscribe`](SharedGraph::subscribe)
/// to get a receiver.
pub struct SharedGraph {
    inner: Arc<RwLock<EntityGraph>>,
    tx: broadcast::Sender<u64>,
}

impl SharedGraph {
    /// Wrap an `EntityGraph` in a thread-safe handle.
    pub fn new(graph: EntityGraph) -> Self {
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            inner: Arc::new(RwLock::new(graph)),
            tx,
        }
    }

    /// Subscribe to graph change notifications.
    ///
    /// Returns a receiver that yields the new graph version after each
    /// write operation (add, update, remove).
    pub fn subscribe(&self) -> broadcast::Receiver<u64> {
        self.tx.subscribe()
    }

    /// Number of active subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }

    /// Execute a closure with shared (read) access to the graph.
    pub fn read<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&EntityGraph) -> R,
    {
        let guard = self.inner.read();
        f(&guard)
    }

    /// Execute a closure with exclusive (write) access to the graph.
    pub fn write<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut EntityGraph) -> R,
    {
        let mut guard = self.inner.write();
        f(&mut guard)
    }

    /// Execute a write closure and broadcast the new version if it changed.
    fn write_and_notify<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut EntityGraph) -> R,
    {
        let (result, version) = {
            let mut guard = self.inner.write();
            let v_before = guard.version();
            let result = f(&mut guard);
            let v_after = guard.version();
            (
                result,
                if v_after != v_before {
                    Some(v_after)
                } else {
                    None
                },
            )
        };
        // Send outside the lock to avoid holding it during broadcast.
        if let Some(v) = version {
            let _ = self.tx.send(v);
        }
        result
    }

    // ── Convenience methods ──

    /// Add an entity. See [`EntityGraph::add`].
    pub fn add(&self, entity: HDict) -> Result<String, GraphError> {
        self.write_and_notify(|g| g.add(entity))
    }

    /// Get an entity by ref value.
    ///
    /// Returns an owned clone because the read lock is released before
    /// the caller uses the value.
    pub fn get(&self, ref_val: &str) -> Option<HDict> {
        self.read(|g| g.get(ref_val).cloned())
    }

    /// Update an entity. See [`EntityGraph::update`].
    pub fn update(&self, ref_val: &str, changes: HDict) -> Result<(), GraphError> {
        self.write_and_notify(|g| g.update(ref_val, changes))
    }

    /// Remove an entity. See [`EntityGraph::remove`].
    pub fn remove(&self, ref_val: &str) -> Result<HDict, GraphError> {
        self.write_and_notify(|g| g.remove(ref_val))
    }

    /// Run a filter expression and return a grid.
    pub fn read_filter(&self, filter_expr: &str, limit: usize) -> Result<HGrid, GraphError> {
        self.read(|g| g.read(filter_expr, limit))
    }

    /// Number of entities.
    pub fn len(&self) -> usize {
        self.read(|g| g.len())
    }

    /// Returns `true` if the graph has no entities.
    pub fn is_empty(&self) -> bool {
        self.read(|g| g.is_empty())
    }

    /// Return all entities as owned clones.
    pub fn all_entities(&self) -> Vec<HDict> {
        self.read(|g| g.all().into_iter().cloned().collect())
    }

    /// Check if an entity with the given ref value exists.
    pub fn contains(&self, ref_val: &str) -> bool {
        self.read(|g| g.contains(ref_val))
    }

    /// Current graph version.
    pub fn version(&self) -> u64 {
        self.read(|g| g.version())
    }

    /// Run a filter and return matching entity dicts (cloned).
    pub fn read_all(&self, filter_expr: &str, limit: usize) -> Result<Vec<HDict>, GraphError> {
        self.read(|g| {
            g.read_all(filter_expr, limit)
                .map(|refs| refs.into_iter().cloned().collect())
        })
    }

    /// Get ref values that the given entity points to.
    pub fn refs_from(&self, ref_val: &str, ref_type: Option<&str>) -> Vec<String> {
        self.read(|g| g.refs_from(ref_val, ref_type))
    }

    /// Get ref values of entities that point to the given entity.
    pub fn refs_to(&self, ref_val: &str, ref_type: Option<&str>) -> Vec<String> {
        self.read(|g| g.refs_to(ref_val, ref_type))
    }

    /// Get changelog entries since a given version.
    ///
    /// Returns `Err(ChangelogGap)` if the requested version has been evicted.
    pub fn changes_since(
        &self,
        version: u64,
    ) -> Result<Vec<super::changelog::GraphDiff>, super::changelog::ChangelogGap> {
        self.read(|g| {
            g.changes_since(version)
                .map(|refs| refs.into_iter().cloned().collect())
        })
    }

    /// Find all entities that structurally fit a spec/type name.
    ///
    /// Returns owned clones. See [`EntityGraph::entities_fitting`].
    pub fn entities_fitting(&self, spec_name: &str) -> Vec<HDict> {
        self.read(|g| g.entities_fitting(spec_name).into_iter().cloned().collect())
    }

    /// Validate all entities against the namespace and check for dangling refs.
    ///
    /// See [`EntityGraph::validate`].
    pub fn validate(&self) -> Vec<ValidationIssue> {
        self.read(|g| g.validate())
    }

    /// Return all edges as `(source_ref, ref_tag, target_ref)` tuples.
    pub fn all_edges(&self) -> Vec<(String, String, String)> {
        self.read(|g| g.all_edges())
    }

    /// BFS neighborhood: entities and edges within `hops` of `ref_val`.
    pub fn neighbors(
        &self,
        ref_val: &str,
        hops: usize,
        ref_types: Option<&[&str]>,
    ) -> (Vec<HDict>, Vec<(String, String, String)>) {
        self.read(|g| {
            let (entities, edges) = g.neighbors(ref_val, hops, ref_types);
            (entities.into_iter().cloned().collect(), edges)
        })
    }

    /// BFS shortest path from `from` to `to`.
    pub fn shortest_path(&self, from: &str, to: &str) -> Vec<String> {
        self.read(|g| g.shortest_path(from, to))
    }

    /// Subtree rooted at `root` up to `max_depth` levels.
    ///
    /// Returns entities with their depth from root.
    pub fn subtree(&self, root: &str, max_depth: usize) -> Vec<(HDict, usize)> {
        self.read(|g| {
            g.subtree(root, max_depth)
                .into_iter()
                .map(|(e, d)| (e.clone(), d))
                .collect()
        })
    }

    /// Walk a chain of ref tags. See [`EntityGraph::ref_chain`].
    pub fn ref_chain(&self, ref_val: &str, ref_tags: &[&str]) -> Vec<HDict> {
        self.read(|g| {
            g.ref_chain(ref_val, ref_tags)
                .into_iter()
                .cloned()
                .collect()
        })
    }

    /// Resolve the site for any entity. See [`EntityGraph::site_for`].
    pub fn site_for(&self, ref_val: &str) -> Option<HDict> {
        self.read(|g| g.site_for(ref_val).cloned())
    }

    /// All direct children of an entity. See [`EntityGraph::children`].
    pub fn children(&self, ref_val: &str) -> Vec<HDict> {
        self.read(|g| g.children(ref_val).into_iter().cloned().collect())
    }

    /// All points for an equip, optionally filtered. See [`EntityGraph::equip_points`].
    pub fn equip_points(&self, equip_ref: &str, filter: Option<&str>) -> Vec<HDict> {
        self.read(|g| {
            g.equip_points(equip_ref, filter)
                .into_iter()
                .cloned()
                .collect()
        })
    }

    /// Build a hierarchy tree. See [`EntityGraph::hierarchy_tree`].
    pub fn hierarchy_tree(&self, root: &str, max_depth: usize) -> Option<HierarchyNode> {
        self.read(|g| g.hierarchy_tree(root, max_depth))
    }

    /// Classify an entity. See [`EntityGraph::classify`].
    pub fn classify(&self, ref_val: &str) -> Option<String> {
        self.read(|g| g.classify(ref_val))
    }
}

impl Default for SharedGraph {
    fn default() -> Self {
        Self::new(EntityGraph::new())
    }
}

impl Clone for SharedGraph {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            tx: self.tx.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kinds::{HRef, Kind};

    fn make_site(id: &str) -> HDict {
        let mut d = HDict::new();
        d.set("id", Kind::Ref(HRef::from_val(id)));
        d.set("site", Kind::Marker);
        d.set("dis", Kind::Str(format!("Site {id}")));
        d
    }

    #[test]
    fn thread_safe_add_get() {
        let sg = SharedGraph::new(EntityGraph::new());
        sg.add(make_site("site-1")).unwrap();

        let entity = sg.get("site-1").unwrap();
        assert!(entity.has("site"));
    }

    #[test]
    fn concurrent_read_access() {
        let sg = SharedGraph::new(EntityGraph::new());
        sg.add(make_site("site-1")).unwrap();

        // Multiple reads at the "same time" via clone.
        let sg2 = sg.clone();

        let entity1 = sg.get("site-1");
        let entity2 = sg2.get("site-1");
        assert!(entity1.is_some());
        assert!(entity2.is_some());
    }

    #[test]
    fn clone_shares_state() {
        let sg = SharedGraph::new(EntityGraph::new());
        let sg2 = sg.clone();

        sg.add(make_site("site-1")).unwrap();

        // sg2 should see the entity added via sg.
        assert!(sg2.get("site-1").is_some());
        assert_eq!(sg2.len(), 1);
    }

    #[test]
    fn convenience_methods() {
        let sg = SharedGraph::new(EntityGraph::new());
        assert!(sg.is_empty());
        assert_eq!(sg.version(), 0);

        sg.add(make_site("site-1")).unwrap();
        assert_eq!(sg.len(), 1);
        assert_eq!(sg.version(), 1);

        let mut changes = HDict::new();
        changes.set("dis", Kind::Str("Updated".into()));
        sg.update("site-1", changes).unwrap();
        assert_eq!(sg.version(), 2);

        let grid = sg.read_filter("site", 0).unwrap();
        assert_eq!(grid.len(), 1);

        sg.remove("site-1").unwrap();
        assert!(sg.is_empty());
    }

    #[test]
    fn concurrent_writes_from_threads() {
        use std::thread;

        let sg = SharedGraph::new(EntityGraph::new());
        let mut handles = Vec::new();

        for i in 0..10 {
            let sg_clone = sg.clone();
            handles.push(thread::spawn(move || {
                let id = format!("site-{i}");
                sg_clone.add(make_site(&id)).unwrap();
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(sg.len(), 10);
    }

    #[test]
    fn contains_check() {
        let sg = SharedGraph::new(EntityGraph::new());
        sg.add(make_site("site-1")).unwrap();
        assert!(sg.contains("site-1"));
        assert!(!sg.contains("site-2"));
    }

    #[test]
    fn default_creates_empty() {
        let sg = SharedGraph::default();
        assert!(sg.is_empty());
        assert_eq!(sg.len(), 0);
        assert_eq!(sg.version(), 0);
    }

    #[test]
    fn read_all_filter() {
        let sg = SharedGraph::new(EntityGraph::new());
        sg.add(make_site("site-1")).unwrap();
        sg.add(make_site("site-2")).unwrap();

        let mut equip = HDict::new();
        equip.set("id", Kind::Ref(HRef::from_val("equip-1")));
        equip.set("equip", Kind::Marker);
        equip.set("siteRef", Kind::Ref(HRef::from_val("site-1")));
        sg.add(equip).unwrap();

        let results = sg.read_all("site", 0).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn concurrent_reads_from_threads() {
        use std::thread;

        let sg = SharedGraph::new(EntityGraph::new());
        for i in 0..20 {
            sg.add(make_site(&format!("site-{i}"))).unwrap();
        }

        let mut handles = Vec::new();
        for _ in 0..8 {
            let sg_clone = sg.clone();
            handles.push(thread::spawn(move || {
                assert_eq!(sg_clone.len(), 20);
                for i in 0..20 {
                    assert!(sg_clone.contains(&format!("site-{i}")));
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }
    }

    #[test]
    fn concurrent_read_write_mix() {
        use std::thread;

        let sg = SharedGraph::new(EntityGraph::new());
        // Pre-populate
        for i in 0..5 {
            sg.add(make_site(&format!("site-{i}"))).unwrap();
        }

        let mut handles = Vec::new();

        // Writer thread: add more entities
        let sg_writer = sg.clone();
        handles.push(thread::spawn(move || {
            for i in 5..15 {
                sg_writer.add(make_site(&format!("site-{i}"))).unwrap();
            }
        }));

        // Reader threads: read existing entities
        for _ in 0..4 {
            let sg_reader = sg.clone();
            handles.push(thread::spawn(move || {
                // Just verify no panics and consistent reads
                let _len = sg_reader.len();
                for i in 0..5 {
                    let _entity = sg_reader.get(&format!("site-{i}"));
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(sg.len(), 15);
    }

    #[test]
    fn version_tracking_across_operations() {
        let sg = SharedGraph::new(EntityGraph::new());
        assert_eq!(sg.version(), 0);

        sg.add(make_site("site-1")).unwrap();
        assert_eq!(sg.version(), 1);

        let mut changes = HDict::new();
        changes.set("dis", Kind::Str("Updated".into()));
        sg.update("site-1", changes).unwrap();
        assert_eq!(sg.version(), 2);

        sg.remove("site-1").unwrap();
        assert_eq!(sg.version(), 3);
    }

    #[test]
    fn refs_from_and_to() {
        let sg = SharedGraph::new(EntityGraph::new());
        sg.add(make_site("site-1")).unwrap();

        let mut equip = HDict::new();
        equip.set("id", Kind::Ref(HRef::from_val("equip-1")));
        equip.set("equip", Kind::Marker);
        equip.set("siteRef", Kind::Ref(HRef::from_val("site-1")));
        sg.add(equip).unwrap();

        let targets = sg.refs_from("equip-1", None);
        assert_eq!(targets, vec!["site-1".to_string()]);

        let sources = sg.refs_to("site-1", None);
        assert_eq!(sources.len(), 1);
    }

    #[test]
    fn changes_since_through_shared() {
        let sg = SharedGraph::new(EntityGraph::new());
        sg.add(make_site("site-1")).unwrap();
        sg.add(make_site("site-2")).unwrap();

        let changes = sg.changes_since(0).unwrap();
        assert_eq!(changes.len(), 2);

        let changes = sg.changes_since(1).unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].ref_val, "site-2");
    }

    #[test]
    fn subscribe_receives_versions() {
        let sg = SharedGraph::new(EntityGraph::new());
        let mut rx = sg.subscribe();
        assert_eq!(sg.subscriber_count(), 1);

        sg.add(make_site("site-1")).unwrap();
        sg.add(make_site("site-2")).unwrap();

        // Receiver should have the two versions.
        assert_eq!(rx.try_recv().unwrap(), 1);
        assert_eq!(rx.try_recv().unwrap(), 2);
        assert!(rx.try_recv().is_err()); // no more
    }

    #[test]
    fn broadcast_on_update_and_remove() {
        let sg = SharedGraph::new(EntityGraph::new());
        sg.add(make_site("site-1")).unwrap();

        let mut rx = sg.subscribe();

        let mut changes = HDict::new();
        changes.set("dis", Kind::Str("Updated".into()));
        sg.update("site-1", changes).unwrap();
        sg.remove("site-1").unwrap();

        assert_eq!(rx.try_recv().unwrap(), 2); // update
        assert_eq!(rx.try_recv().unwrap(), 3); // remove
    }

    #[test]
    fn no_subscribers_does_not_panic() {
        let sg = SharedGraph::new(EntityGraph::new());
        // No subscribers — write should still succeed.
        sg.add(make_site("site-1")).unwrap();
        assert_eq!(sg.len(), 1);
    }
}
