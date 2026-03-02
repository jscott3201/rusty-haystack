use std::time::Instant;

use dashmap::DashMap;

use crate::domain_scope::DomainScope;

use super::affinity::ConnectorAffinity;
use super::working_set::WorkingSetCache;

/// Per-session profile combining all session optimization state.
pub struct SessionProfile {
    pub user_id: String,
    pub domain_scope: DomainScope,
    pub affinity: ConnectorAffinity,
    pub working_set: WorkingSetCache,
    pub created_at: Instant,
    pub last_activity: Instant,
}

impl SessionProfile {
    pub fn new(user_id: String, domain_scope: DomainScope) -> Self {
        Self {
            user_id,
            domain_scope,
            affinity: ConnectorAffinity::new(),
            working_set: WorkingSetCache::new(256),
            created_at: Instant::now(),
            last_activity: Instant::now(),
        }
    }

    /// Update last activity timestamp.
    pub fn touch(&mut self) {
        self.last_activity = Instant::now();
    }

    /// Check if this session has been idle for longer than the given duration.
    pub fn is_idle(&self, max_idle_secs: u64) -> bool {
        self.last_activity.elapsed().as_secs() >= max_idle_secs
    }
}

/// Registry of active sessions.
pub struct SessionRegistry {
    profiles: DashMap<u64, SessionProfile>,
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self {
            profiles: DashMap::new(),
        }
    }

    /// Create a new session profile.
    pub fn create(&self, session_id: u64, user_id: String, scope: DomainScope) {
        self.profiles
            .insert(session_id, SessionProfile::new(user_id, scope));
    }

    /// Get a session profile for reading.
    pub fn get(
        &self,
        session_id: u64,
    ) -> Option<dashmap::mapref::one::Ref<'_, u64, SessionProfile>> {
        self.profiles.get(&session_id)
    }

    /// Get a mutable reference to a session profile.
    pub fn get_mut(
        &self,
        session_id: u64,
    ) -> Option<dashmap::mapref::one::RefMut<'_, u64, SessionProfile>> {
        self.profiles.get_mut(&session_id)
    }

    /// Remove a session.
    pub fn remove(&self, session_id: u64) -> Option<(u64, SessionProfile)> {
        self.profiles.remove(&session_id)
    }

    /// Evict sessions idle for longer than max_idle_secs. Returns count evicted.
    pub fn evict_idle(&self, max_idle_secs: u64) -> usize {
        let to_remove: Vec<u64> = self
            .profiles
            .iter()
            .filter(|entry| entry.value().is_idle(max_idle_secs))
            .map(|entry| *entry.key())
            .collect();
        let count = to_remove.len();
        for id in to_remove {
            self.profiles.remove(&id);
        }
        count
    }

    /// Number of active sessions.
    pub fn len(&self) -> usize {
        self.profiles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty()
    }
}

impl Default for SessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_profile_creation() {
        let profile = SessionProfile::new("user1".into(), DomainScope::all());
        assert_eq!(profile.user_id, "user1");
        assert!(!profile.is_idle(3600));
    }

    #[test]
    fn session_touch_updates_activity() {
        let mut profile = SessionProfile::new("user1".into(), DomainScope::all());
        let before = profile.last_activity;
        // Small sleep to ensure time advances
        std::thread::sleep(std::time::Duration::from_millis(10));
        profile.touch();
        assert!(profile.last_activity > before);
    }

    #[test]
    fn session_idle_detection() {
        let mut profile = SessionProfile::new("user1".into(), DomainScope::all());
        // Not idle for a long threshold
        assert!(!profile.is_idle(3600));
        // Force last_activity into the past
        profile.last_activity = Instant::now() - std::time::Duration::from_secs(100);
        assert!(profile.is_idle(50));
        assert!(!profile.is_idle(200));
    }

    #[test]
    fn registry_create_get_remove() {
        let reg = SessionRegistry::new();
        assert!(reg.is_empty());

        reg.create(1, "user1".into(), DomainScope::all());
        assert_eq!(reg.len(), 1);

        let profile = reg.get(1);
        assert!(profile.is_some());
        assert_eq!(profile.unwrap().user_id, "user1");

        assert!(reg.get(999).is_none());

        let removed = reg.remove(1);
        assert!(removed.is_some());
        assert!(reg.is_empty());
    }

    #[test]
    fn registry_get_mut() {
        let reg = SessionRegistry::new();
        reg.create(1, "user1".into(), DomainScope::all());

        if let Some(mut profile) = reg.get_mut(1) {
            profile.touch();
            profile.affinity.record_hit("conn_a");
        }

        let profile = reg.get(1).unwrap();
        assert_eq!(profile.affinity.owner_of("missing"), None);
    }

    #[test]
    fn registry_evict_idle() {
        let reg = SessionRegistry::new();
        reg.create(1, "active".into(), DomainScope::all());
        reg.create(2, "idle".into(), DomainScope::all());

        // Make session 2 idle
        if let Some(mut profile) = reg.get_mut(2) {
            profile.last_activity = Instant::now() - std::time::Duration::from_secs(100);
        }

        let evicted = reg.evict_idle(50);
        assert_eq!(evicted, 1);
        assert_eq!(reg.len(), 1);
        assert!(reg.get(1).is_some());
        assert!(reg.get(2).is_none());
    }

    #[test]
    fn registry_default() {
        let reg = SessionRegistry::default();
        assert!(reg.is_empty());
    }
}
