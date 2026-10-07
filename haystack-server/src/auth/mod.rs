//! Bounded three-request Haystack SCRAM SHA-256 authentication.
pub mod users;

use base64::{
    Engine,
    engine::general_purpose::{STANDARD as BASE64, URL_SAFE_NO_PAD as OUTER},
};
use haystack_core::auth::{self, DEFAULT_ITERATIONS, ScramCredentials, ScramHandshake};
use hmac::{Hmac, KeyInit, Mac};
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use users::{UserRecord, load_users_from_str, load_users_from_toml};
use uuid::Uuid;
use zeroize::Zeroize;

#[derive(Debug, Clone)]
pub struct AuthUser {
    pub username: String,
    pub permissions: Vec<String>,
}

/// Per-process admission bounds, not a replacement for deployment rate limits.
#[derive(Clone, Copy, Debug)]
pub struct AuthLimits {
    pub max_handshakes: usize,
    pub max_tokens: usize,
    pub handshake_ttl: Duration,
}
impl Default for AuthLimits {
    fn default() -> Self {
        Self {
            max_handshakes: 1024,
            max_tokens: 4096,
            handshake_ttl: Duration::from_secs(60),
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum AuthFailure {
    Rejected,
    Capacity,
}
/// Challenge responses are 401; authenticated responses are 200.
pub enum ScramResponse {
    Challenge(String),
    Authenticated(String),
}
impl std::fmt::Debug for ScramResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ScramResponse { [REDACTED] }")
    }
}

enum Phase {
    AwaitFirst(String),
    AwaitFinal(ScramHandshake),
}
#[derive(Default)]
struct State {
    handshakes: HashMap<String, (Phase, Instant)>,
    tokens: HashMap<String, (AuthUser, Instant)>,
}
/// One lock makes sweep, admission, transition and final consumption atomic.
/// Unknown identities use cheap secret-derived decoys and can never receive a bearer.
pub struct AuthManager {
    users: HashMap<String, UserRecord>,
    state: Mutex<State>,
    token_ttl: Duration,
    limits: AuthLimits,
    server_secret: [u8; 32],
}
impl Drop for AuthManager {
    fn drop(&mut self) {
        self.server_secret.zeroize();
    }
}
impl AuthManager {
    pub fn new(users: HashMap<String, UserRecord>, token_ttl: Duration) -> Self {
        Self {
            users,
            state: Mutex::new(State::default()),
            token_ttl,
            limits: AuthLimits::default(),
            server_secret: rand::RngExt::random(&mut rand::rng()),
        }
    }
    pub fn empty() -> Self {
        Self::new(HashMap::new(), Duration::from_secs(3600))
    }
    pub fn with_token_ttl(mut self, duration: Duration) -> Self {
        self.token_ttl = duration;
        self
    }
    pub fn with_limits(mut self, limits: AuthLimits) -> Self {
        self.limits = limits;
        self
    }
    pub fn from_toml(path: &str) -> Result<Self, String> {
        Ok(Self::new(
            load_users_from_toml(path)?,
            Duration::from_secs(3600),
        ))
    }
    pub fn from_toml_str(content: &str) -> Result<Self, String> {
        Ok(Self::new(
            load_users_from_str(content)?,
            Duration::from_secs(3600),
        ))
    }
    pub fn is_enabled(&self) -> bool {
        !self.users.is_empty()
    }

    fn fake_key(&self, domain: &[u8], username: &str) -> Vec<u8> {
        let mut mac = <Hmac<Sha256>>::new_from_slice(&self.server_secret)
            .expect("HMAC accepts keys of any size");
        mac.update(domain);
        mac.update(&[0]);
        mac.update(username.as_bytes());
        mac.finalize().into_bytes().to_vec()
    }
    fn fake_credentials(&self, username: &str) -> ScramCredentials {
        let mut client_key = self.fake_key(b"client-key", username);
        let stored_key = Sha256::digest(&client_key).to_vec();
        client_key.zeroize();
        ScramCredentials {
            salt: self.fake_key(b"salt", username)[..16].to_vec(),
            iterations: DEFAULT_ITERATIONS,
            stored_key,
            server_key: self.fake_key(b"server-key", username),
        }
    }
    fn sweep(&self, state: &mut State, now: Instant) {
        state.handshakes.retain(|_, (_, created)| {
            now.saturating_duration_since(*created) < self.limits.handshake_ttl
        });
        state
            .tokens
            .retain(|_, (_, created)| now.saturating_duration_since(*created) < self.token_ttl);
    }
    /// Admit HELLO without PBKDF2 or a client-first transcript.
    pub fn handle_hello(&self, username: &str) -> Result<String, AuthFailure> {
        self.hello_with_clock(username, Instant::now)
    }
    #[cfg(test)]
    fn hello_at(&self, username: &str, now: Instant) -> Result<String, AuthFailure> {
        self.hello_with_clock(username, || now)
    }
    fn hello_with_clock(
        &self,
        username: &str,
        clock: impl FnOnce() -> Instant,
    ) -> Result<String, AuthFailure> {
        auth::validate_username(username).map_err(|_| AuthFailure::Rejected)?;
        let mut state = self.state.lock();
        let now = clock();
        self.sweep(&mut state, now);
        if state.handshakes.len() >= self.limits.max_handshakes {
            return Err(AuthFailure::Capacity);
        }
        let token = Uuid::new_v4().to_string();
        state
            .handshakes
            .insert(token.clone(), (Phase::AwaitFirst(username.into()), now));
        Ok(auth::format_www_authenticate(Some(&token), None))
    }
    /// Consume the issued token once. Client-first rotates it without extending TTL.
    pub fn handle_scram(
        &self,
        token: Option<&str>,
        data: &str,
    ) -> Result<ScramResponse, AuthFailure> {
        self.scram_with_clock(token, data, Instant::now)
    }
    #[cfg(test)]
    fn scram_at(
        &self,
        token: Option<&str>,
        data: &str,
        now: Instant,
    ) -> Result<ScramResponse, AuthFailure> {
        self.scram_with_clock(token, data, || now)
    }
    fn scram_with_clock(
        &self,
        token: Option<&str>,
        data: &str,
        clock: impl FnOnce() -> Instant,
    ) -> Result<ScramResponse, AuthFailure> {
        let token = token.ok_or(AuthFailure::Rejected)?;
        let mut state = self.state.lock();
        // A call can wait across expiry. Time belongs to the locked decision,
        // not the earlier attempt to acquire the state lock.
        let now = clock();
        self.sweep(&mut state, now);
        let (phase, created) = state
            .handshakes
            .remove(token)
            .ok_or(AuthFailure::Rejected)?;
        match phase {
            Phase::AwaitFirst(username) => {
                let fake;
                let credentials = if let Some(record) = self.users.get(&username) {
                    &record.credentials
                } else {
                    fake = self.fake_credentials(&username);
                    &fake
                };
                let (handshake, data) = auth::server_first_message(&username, data, credentials)
                    .map_err(|_| AuthFailure::Rejected)?;
                let token = Uuid::new_v4().to_string();
                state
                    .handshakes
                    .insert(token.clone(), (Phase::AwaitFinal(handshake), created));
                Ok(ScramResponse::Challenge(auth::format_www_authenticate(
                    Some(&token),
                    Some(&data),
                )))
            }
            Phase::AwaitFinal(handshake) => {
                let signature = auth::server_verify_final(&handshake, data)
                    .map_err(|_| AuthFailure::Rejected)?;
                // Membership is required even if a decoy proof is cryptographically correct.
                let record = self
                    .users
                    .get(&handshake.username)
                    .ok_or(AuthFailure::Rejected)?;
                if state.tokens.len() >= self.limits.max_tokens {
                    return Err(AuthFailure::Capacity);
                }
                let token = Uuid::new_v4().to_string();
                let user = AuthUser {
                    username: handshake.username.clone(),
                    permissions: record.permissions.clone(),
                };
                state.tokens.insert(token.clone(), (user, now));
                let final_data = OUTER.encode(format!("v={}", BASE64.encode(signature)));
                Ok(ScramResponse::Authenticated(auth::format_auth_info(
                    &token,
                    &final_data,
                )))
            }
        }
    }
    pub fn validate_token(&self, token: &str) -> Option<AuthUser> {
        self.validate_with_clock(token, Instant::now)
    }
    #[cfg(test)]
    fn validate_at(&self, token: &str, now: Instant) -> Option<AuthUser> {
        self.validate_with_clock(token, || now)
    }
    fn validate_with_clock(
        &self,
        token: &str,
        clock: impl FnOnce() -> Instant,
    ) -> Option<AuthUser> {
        let mut state = self.state.lock();
        let now = clock();
        self.sweep(&mut state, now);
        state.tokens.get(token).map(|(user, _)| user.clone())
    }
    pub fn revoke_token(&self, token: &str) -> bool {
        self.state.lock().tokens.remove(token).is_some()
    }
    #[doc(hidden)]
    pub fn inject_token(&self, token: String, user: AuthUser) {
        let mut state = self.state.lock();
        let now = Instant::now();
        self.sweep(&mut state, now);
        if state.tokens.len() < self.limits.max_tokens {
            state.tokens.insert(token, (user, now));
        }
    }
    pub fn check_permission(user: &AuthUser, required: &str) -> bool {
        user.permissions
            .iter()
            .any(|p| p == "admin" || p == required)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn manager() -> AuthManager {
        let credentials = auth::derive_credentials("pencil", b"synthetic-salt", 4096);
        AuthManager::new(
            HashMap::from([(
                "user".into(),
                UserRecord {
                    credentials,
                    permissions: vec!["read".into()],
                },
            )]),
            Duration::from_secs(10),
        )
    }
    fn fields(header: &str) -> HashMap<String, &str> {
        auth::parse_auth_parameters(header.strip_prefix("SCRAM ").unwrap_or(header)).unwrap()
    }
    fn ready(mgr: &AuthManager, name: &str, now: Instant) -> (String, String, String) {
        let hello = mgr.hello_at(name, now).unwrap();
        let token = fields(&hello)["handshaketoken"].to_string();
        let (_, first) = auth::client_first_message(name);
        let ScramResponse::Challenge(challenge) = mgr.scram_at(Some(&token), &first, now).unwrap()
        else {
            panic!("expected challenge")
        };
        assert_eq!(
            mgr.scram_at(Some(&token), &first, now).err(),
            Some(AuthFailure::Rejected)
        );
        let parsed = fields(&challenge);
        (
            parsed["handshaketoken"].into(),
            first,
            parsed["data"].into(),
        )
    }
    #[test]
    fn unknown_user_correct_fake_proof_never_issues_bearer() {
        let mgr = manager();
        let now = Instant::now();
        let (token, first, server) = ready(&mgr, "ghost", now);
        let bare = auth::decode_auth_data(&first).unwrap();
        let server = auth::decode_auth_data(&server).unwrap();
        let nonce = server
            .split(',')
            .next()
            .unwrap()
            .strip_prefix("r=")
            .unwrap();
        let without = format!("c=biws,r={nonce}");
        let transcript = format!("{},{server},{without}", &bare[3..]);
        let credentials = mgr.fake_credentials("ghost");
        let mut mac = <Hmac<Sha256>>::new_from_slice(&credentials.stored_key).unwrap();
        mac.update(transcript.as_bytes());
        let signature = mac.finalize().into_bytes();
        let client_key = mgr.fake_key(b"client-key", "ghost");
        let proof: Vec<_> = client_key
            .iter()
            .zip(signature.iter())
            .map(|(a, b)| a ^ b)
            .collect();
        let data = OUTER.encode(format!("{without},p={}", BASE64.encode(proof)));
        // First prove this is a valid decoy proof, then prove identity admission rejects it.
        {
            let state = mgr.state.lock();
            let Phase::AwaitFinal(hs) = &state.handshakes[&token].0 else {
                panic!()
            };
            assert!(auth::server_verify_final(hs, &data).is_ok());
        }
        assert_eq!(
            mgr.scram_at(Some(&token), &data, now).err(),
            Some(AuthFailure::Rejected)
        );
        assert!(mgr.state.lock().tokens.is_empty());
    }
    #[test]
    fn final_consumed_once_under_concurrency() {
        let mgr = manager();
        let now = Instant::now();
        let (token, first, server) = ready(&mgr, "user", now);
        let (proof, _) = auth::client_final_message("pencil", &first, &server).unwrap();
        std::thread::scope(|scope| {
            let a = scope.spawn(|| mgr.handle_scram(Some(&token), &proof));
            let b = scope.spawn(|| mgr.handle_scram(Some(&token), &proof));
            let results = [a.join().unwrap(), b.join().unwrap()];
            assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        });
        assert_eq!(mgr.state.lock().tokens.len(), 1);
    }
    #[test]
    fn expired_states_are_swept_at_admission_and_transition_keeps_original_age() {
        let mgr = manager().with_limits(AuthLimits {
            max_handshakes: 1,
            max_tokens: 1,
            handshake_ttl: Duration::from_secs(2),
        });
        let now = Instant::now();
        let hello = mgr.hello_at("user", now).unwrap();
        assert_eq!(mgr.hello_at("user", now).err(), Some(AuthFailure::Capacity));
        let (_, first) = auth::client_first_message("user");
        let ScramResponse::Challenge(challenge) = mgr
            .scram_at(
                Some(fields(&hello)["handshaketoken"]),
                &first,
                now + Duration::from_secs(1),
            )
            .unwrap()
        else {
            panic!()
        };
        let p = fields(&challenge);
        let (proof, _) = auth::client_final_message("pencil", &first, p["data"]).unwrap();
        assert_eq!(
            mgr.scram_at(
                Some(p["handshaketoken"]),
                &proof,
                now + Duration::from_secs(2)
            )
            .err(),
            Some(AuthFailure::Rejected)
        );
        mgr.hello_at("user", now + Duration::from_secs(2)).unwrap();
        mgr.hello_at("user", now + Duration::from_secs(4)).unwrap();
        assert_eq!(mgr.state.lock().handshakes.len(), 1);
    }
    #[test]
    fn bearer_capacity_and_expiry_are_atomic_even_without_token_reads() {
        let mgr = manager().with_limits(AuthLimits {
            max_tokens: 1,
            ..AuthLimits::default()
        });
        let now = Instant::now();
        let (token, first, server) = ready(&mgr, "user", now);
        let (proof, _) = auth::client_final_message("pencil", &first, &server).unwrap();
        let ScramResponse::Authenticated(info) = mgr.scram_at(Some(&token), &proof, now).unwrap()
        else {
            panic!()
        };
        let bearer = fields(&info)["authtoken"].to_string();
        assert!(mgr.validate_at(&bearer, now).is_some());
        let (token, first, server) = ready(&mgr, "user", now);
        let (proof, _) = auth::client_final_message("pencil", &first, &server).unwrap();
        assert_eq!(
            mgr.scram_at(Some(&token), &proof, now).err(),
            Some(AuthFailure::Capacity)
        );
        let later = now + Duration::from_secs(10);
        let (token, first, server) = ready(&mgr, "user", later);
        let (proof, _) = auth::client_final_message("pencil", &first, &server).unwrap();
        assert!(mgr.scram_at(Some(&token), &proof, later).is_ok());
        assert!(mgr.validate_at(&bearer, later).is_none());
        assert_eq!(mgr.state.lock().tokens.len(), 1);
    }
    #[test]
    fn missing_wrong_stage_and_wrong_identity_are_rejected() {
        let mgr = manager();
        let now = Instant::now();
        assert_eq!(
            mgr.handle_scram(None, "x").err(),
            Some(AuthFailure::Rejected)
        );
        for data in [
            OUTER.encode("c=biws,r=n,p=eA=="),
            auth::client_first_message("other").1,
        ] {
            let hello = mgr.hello_at("user", now).unwrap();
            let token = fields(&hello)["handshaketoken"];
            assert_eq!(
                mgr.scram_at(Some(token), &data, now).err(),
                Some(AuthFailure::Rejected)
            );
            assert_eq!(
                mgr.scram_at(Some(token), &data, now).err(),
                Some(AuthFailure::Rejected)
            );
        }
        let (token, first, _) = ready(&mgr, "user", now);
        assert_eq!(
            mgr.scram_at(Some(&token), &first, now).err(),
            Some(AuthFailure::Rejected)
        );
    }
    #[test]
    fn concurrent_hello_admission_never_exceeds_capacity() {
        let mgr = manager().with_limits(AuthLimits {
            max_handshakes: 2,
            ..AuthLimits::default()
        });
        let accepted = std::thread::scope(|scope| {
            let tasks: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| mgr.handle_hello("user")))
                .collect();
            tasks
                .into_iter()
                .map(|t| t.join().unwrap())
                .filter(Result::is_ok)
                .count()
        });
        assert_eq!(accepted, 2);
        assert_eq!(mgr.state.lock().handshakes.len(), 2);
    }

    #[test]
    fn final_proof_waiting_on_state_lock_expires_before_admission() {
        let ttl = Duration::from_millis(200);
        let mgr = manager().with_limits(AuthLimits {
            handshake_ttl: ttl,
            ..AuthLimits::default()
        });
        let created = Instant::now();
        let (token, first, server) = ready(&mgr, "user", created);
        let (proof, _) = auth::client_final_message("pencil", &first, &server).unwrap();
        let expires = created + ttl;
        std::thread::scope(|scope| {
            let guard = mgr.state.lock();
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let mgr = &mgr;
            let worker = scope.spawn(move || {
                started_tx.send(Instant::now()).unwrap();
                mgr.handle_scram(Some(&token), &proof)
            });
            let started = started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
            assert!(
                started < expires,
                "caller must start while proof is eligible"
            );
            std::thread::sleep(
                expires.saturating_duration_since(Instant::now()) + Duration::from_millis(50),
            );
            assert!(
                !worker.is_finished(),
                "public call must wait on the held state lock"
            );
            drop(guard);
            assert_eq!(worker.join().unwrap().err(), Some(AuthFailure::Rejected));
        });
        assert!(mgr.state.lock().tokens.is_empty());
    }

    #[test]
    fn bearer_waiting_on_state_lock_expires_before_validation() {
        let ttl = Duration::from_millis(200);
        let mgr = manager().with_token_ttl(ttl);
        mgr.inject_token(
            "waiting-token".into(),
            AuthUser {
                username: "user".into(),
                permissions: vec!["read".into()],
            },
        );
        std::thread::scope(|scope| {
            let guard = mgr.state.lock();
            let expires = guard.tokens["waiting-token"].1 + ttl;
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let mgr = &mgr;
            let worker = scope.spawn(move || {
                started_tx.send(Instant::now()).unwrap();
                mgr.validate_token("waiting-token")
            });
            let started = started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
            assert!(
                started < expires,
                "caller must start while bearer is eligible"
            );
            std::thread::sleep(
                expires.saturating_duration_since(Instant::now()) + Duration::from_millis(50),
            );
            assert!(
                !worker.is_finished(),
                "public call must wait on the held state lock"
            );
            drop(guard);
            assert!(worker.join().unwrap().is_none());
        });
        assert!(mgr.state.lock().tokens.is_empty());
    }

    #[test]
    fn permissions_and_revocation() {
        let mgr = manager();
        let user = AuthUser {
            username: "u".into(),
            permissions: vec!["read".into()],
        };
        assert!(AuthManager::check_permission(&user, "read"));
        assert!(!AuthManager::check_permission(&user, "write"));
        mgr.inject_token("token".into(), user);
        assert!(mgr.validate_token("token").is_some());
        assert!(mgr.revoke_token("token"));
        assert!(!mgr.revoke_token("token"));
    }
}
