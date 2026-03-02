// GraphSubscriber — async helper for consuming graph change notifications.

use tokio::sync::broadcast;

use super::changelog::{ChangelogGap, GraphDiff};
use super::shared::SharedGraph;

/// Async helper that pairs a [`SharedGraph`] with a broadcast receiver
/// to yield batches of [`GraphDiff`] entries whenever the graph changes.
///
/// # Example
///
/// ```ignore
/// let subscriber = GraphSubscriber::new(graph.clone());
/// loop {
///     match subscriber.next_batch().await {
///         Ok(diffs) => { /* process diffs */ }
///         Err(gap) => { /* full resync needed */ }
///     }
/// }
/// ```
pub struct GraphSubscriber {
    graph: SharedGraph,
    rx: broadcast::Receiver<u64>,
    last_version: u64,
}

impl GraphSubscriber {
    /// Create a new subscriber starting from the graph's current version.
    pub fn new(graph: SharedGraph) -> Self {
        let rx = graph.subscribe();
        let last_version = graph.version();
        Self {
            graph,
            rx,
            last_version,
        }
    }

    /// Create a subscriber starting from a specific version.
    ///
    /// Useful for resuming after a reconnect.
    pub fn from_version(graph: SharedGraph, version: u64) -> Self {
        let rx = graph.subscribe();
        Self {
            graph,
            rx,
            last_version: version,
        }
    }

    /// Wait for the next batch of changes and return them.
    ///
    /// Blocks (async) until at least one write occurs, then returns all
    /// diffs since the last consumed version. Returns `Err(ChangelogGap)`
    /// if the subscriber has fallen too far behind.
    pub async fn next_batch(&mut self) -> Result<Vec<GraphDiff>, ChangelogGap> {
        // Wait for at least one version notification.
        // Coalesce: drain any additional pending notifications.
        let mut _latest = match self.rx.recv().await {
            Ok(v) => v,
            Err(broadcast::error::RecvError::Lagged(_)) => {
                // Missed messages — still try to get diffs from changelog.
                self.graph.version()
            }
            Err(broadcast::error::RecvError::Closed) => {
                // Channel closed — return current state.
                return Ok(Vec::new());
            }
        };

        // Drain any buffered notifications to coalesce into one batch.
        while let Ok(v) = self.rx.try_recv() {
            _latest = v;
        }

        let diffs = self.graph.changes_since(self.last_version)?;
        if let Some(last) = diffs.last() {
            self.last_version = last.version;
        }
        Ok(diffs)
    }

    /// The last version this subscriber has consumed.
    pub fn version(&self) -> u64 {
        self.last_version
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::HDict;
    use crate::graph::EntityGraph;
    use crate::kinds::{HRef, Kind};

    fn make_site(id: &str) -> HDict {
        let mut d = HDict::new();
        d.set("id", Kind::Ref(HRef::from_val(id)));
        d.set("site", Kind::Marker);
        d
    }

    #[tokio::test]
    async fn subscriber_receives_diffs() {
        let sg = SharedGraph::new(EntityGraph::new());
        let mut sub = GraphSubscriber::new(sg.clone());
        assert_eq!(sub.version(), 0);

        sg.add(make_site("site-1")).unwrap();

        let diffs = sub.next_batch().await.unwrap();
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].ref_val, "site-1");
        assert_eq!(sub.version(), 1);
    }

    #[tokio::test]
    async fn subscriber_coalesces_batches() {
        let sg = SharedGraph::new(EntityGraph::new());
        let mut sub = GraphSubscriber::new(sg.clone());

        // Add multiple entities before subscriber reads.
        sg.add(make_site("site-1")).unwrap();
        sg.add(make_site("site-2")).unwrap();
        sg.add(make_site("site-3")).unwrap();

        // Give broadcast a moment to buffer.
        tokio::task::yield_now().await;

        let diffs = sub.next_batch().await.unwrap();
        assert_eq!(diffs.len(), 3);
        assert_eq!(sub.version(), 3);
    }

    #[tokio::test]
    async fn subscriber_from_version() {
        let sg = SharedGraph::new(EntityGraph::new());
        sg.add(make_site("site-1")).unwrap();
        sg.add(make_site("site-2")).unwrap();

        // Start from version 1, should only get v2 onwards.
        let mut sub = GraphSubscriber::from_version(sg.clone(), 1);

        sg.add(make_site("site-3")).unwrap();

        let diffs = sub.next_batch().await.unwrap();
        assert_eq!(diffs.len(), 2); // site-2 (v2) and site-3 (v3)
        assert_eq!(sub.version(), 3);
    }
}
