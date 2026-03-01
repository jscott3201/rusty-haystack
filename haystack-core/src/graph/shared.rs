// SharedGraph — thread-safe wrapper around EntityGraph using parking_lot RwLock.

use parking_lot::RwLock;
use std::sync::Arc;

use crate::data::{HDict, HGrid};
use crate::ontology::ValidationIssue;

use super::entity_graph::{EntityGraph, GraphError};

/// Thread-safe, clonable handle to an `EntityGraph`.
///
/// Uses `parking_lot::RwLock` for reader-writer locking, allowing
/// concurrent reads with exclusive writes. Cloning shares the
/// underlying graph (via `Arc`).
pub struct SharedGraph {
    inner: Arc<RwLock<EntityGraph>>,
}

impl SharedGraph {
    /// Wrap an `EntityGraph` in a thread-safe handle.
    pub fn new(graph: EntityGraph) -> Self {
        Self {
            inner: Arc::new(RwLock::new(graph)),
        }
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

    // ── Convenience methods ──

    /// Add an entity. See [`EntityGraph::add`].
    pub fn add(&self, entity: HDict) -> Result<String, GraphError> {
        self.write(|g| g.add(entity))
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
        self.write(|g| g.update(ref_val, changes))
    }

    /// Remove an entity. See [`EntityGraph::remove`].
    pub fn remove(&self, ref_val: &str) -> Result<HDict, GraphError> {
        self.write(|g| g.remove(ref_val))
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
    pub fn changes_since(&self, version: u64) -> Vec<super::changelog::GraphDiff> {
        self.read(|g| g.changes_since(version).to_vec())
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

        let changes = sg.changes_since(0);
        assert_eq!(changes.len(), 2);

        let changes = sg.changes_since(1);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].ref_val, "site-2");
    }
}
