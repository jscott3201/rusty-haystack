//! Offline RFC 9068 RS256 access-token verification. No discovery or network I/O.
use crate::{McpError, common::strict_json};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use haystack_app::Principal;
use parking_lot::RwLock;
use ring::signature::{RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{SystemTime, UNIX_EPOCH},
};

/// An explicitly trusted RS256 key, encoded as a PKCS#1 DER RSA public key.
#[derive(Clone)]
pub struct VerificationKey {
    pub kid: String,
    pub public_key_der: Vec<u8>,
}
#[derive(Clone)]
pub struct JwtConfig {
    pub issuer: String,
    pub resource: String,
    pub required_scope: String,
    pub subjects: Vec<String>,
    pub keys: Vec<VerificationKey>,
    pub revoked_jti: Vec<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthError {
    InvalidToken,
    InsufficientScope,
}
/// Trusted, immutable issuer/key/subject policy with a bounded local revocation set.
/// Revocation is checked on every HTTP request. Remote issuer revocations are
/// unknown until the embedding owner calls `revoke` or replaces this verifier.
pub struct JwtVerifier {
    config: JwtConfig,
    keys: BTreeMap<String, Vec<u8>>,
    subjects: BTreeSet<String>,
    revoked: RwLock<BTreeSet<String>>,
}
impl JwtVerifier {
    pub fn new(config: JwtConfig) -> Result<Self, McpError> {
        if !url_ok(&config.issuer)
            || !url_ok(&config.resource)
            || config.required_scope.len() > 128
            || !scope_token(&config.required_scope)
            || !(1..=512).contains(&config.subjects.len())
            || config.subjects.iter().any(|s| !atom(s, 256))
            || !(1..=16).contains(&config.keys.len())
            || config
                .keys
                .iter()
                .any(|k| !atom(&k.kid, 128) || !(256..=2048).contains(&k.public_key_der.len()))
            || config.revoked_jti.len() > 1024
            || config.revoked_jti.iter().any(|s| !atom(s, 256))
        {
            return Err(McpError::Configuration);
        }
        let keys: BTreeMap<_, _> = config
            .keys
            .iter()
            .map(|k| (k.kid.clone(), k.public_key_der.clone()))
            .collect();
        if keys.len() != config.keys.len() {
            return Err(McpError::Configuration);
        }
        Ok(Self {
            subjects: config.subjects.iter().cloned().collect(),
            revoked: RwLock::new(config.revoked_jti.iter().cloned().collect()),
            keys,
            config,
        })
    }
    pub fn revoke(&self, jti: &str) -> Result<(), McpError> {
        let mut revoked = self.revoked.write();
        if !atom(jti, 256) || (revoked.len() >= 1024 && !revoked.contains(jti)) {
            return Err(McpError::Capacity);
        }
        revoked.insert(jti.to_owned());
        Ok(())
    }
    pub fn resource(&self) -> &str {
        &self.config.resource
    }
    pub fn issuer(&self) -> &str {
        &self.config.issuer
    }
    pub fn required_scope(&self) -> &str {
        &self.config.required_scope
    }
    pub fn verify(&self, token: &str) -> Result<Principal, AuthError> {
        use AuthError::InvalidToken;
        if token.len() > 8192 {
            return Err(InvalidToken);
        }
        let segments: Vec<_> = token.split('.').collect();
        let [header, claims, signature] = segments.as_slice() else {
            return Err(InvalidToken);
        };
        let decode = |s: &str| URL_SAFE_NO_PAD.decode(s).map_err(|_| InvalidToken);
        let header: Header =
            serde_json::from_value(strict_json(&decode(header)?, 8).map_err(|_| InvalidToken)?)
                .map_err(|_| InvalidToken)?;
        if header.alg != "RS256" || !matches!(header.typ.as_str(), "at+jwt" | "application/at+jwt")
        {
            return Err(InvalidToken);
        }
        let key = self.keys.get(&header.kid).ok_or(InvalidToken)?;
        let signed = &token[..token.len() - signature.len() - 1];
        UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, key)
            .verify(signed.as_bytes(), &decode(signature)?)
            .map_err(|_| InvalidToken)?;
        let claims: Claims =
            serde_json::from_value(strict_json(&decode(claims)?, 8).map_err(|_| InvalidToken)?)
                .map_err(|_| InvalidToken)?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| InvalidToken)?
            .as_secs();
        if claims.iss != self.config.issuer
            || !claims.aud.matches(&self.config.resource)
            || claims.exp <= now
            || claims.iat > now
            || claims.iat >= claims.exp
            || claims.nbf.is_some_and(|nbf| nbf > now || nbf >= claims.exp)
            || !atom(&claims.sub, 256)
            || !atom(&claims.client_id, 256)
            || !atom(&claims.jti, 256)
            || self.revoked.read().contains(&claims.jti)
        {
            return Err(InvalidToken);
        }
        // RFC 6749 scope is one or more nonempty scope tokens separated by a
        // single ASCII space. Validate the entire signed claim before granting
        // even one permission; a valid token beside malformed text is invalid.
        if claims
            .scope
            .as_ref()
            .is_some_and(|scope| scope.len() > 1024 || !scope.split(' ').all(scope_token))
        {
            return Err(InvalidToken);
        }
        if !self.subjects.contains(&claims.sub)
            || !claims.scope.as_ref().is_some_and(|scope| {
                scope
                    .split(' ')
                    .any(|scope| scope == self.config.required_scope)
            })
        {
            return Err(AuthError::InsufficientScope);
        }
        Ok(Principal::authenticated(
            claims.sub,
            vec![self.config.required_scope.clone()],
        ))
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    alg: String,
    typ: String,
    kid: String,
}
#[derive(Deserialize)]
struct Claims {
    iss: String,
    sub: String,
    aud: Audience,
    exp: u64,
    iat: u64,
    client_id: String,
    jti: String,
    #[serde(default)]
    nbf: Option<u64>,
    #[serde(default, deserialize_with = "present_scope")]
    scope: Option<String>,
}
// Absence is insufficient authority; a present non-string (including null) is
// an invalid claim. Option's default deserializer would conflate null/absence.
fn present_scope<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
}
fn scope_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| matches!(byte, 0x21 | 0x23..=0x5b | 0x5d..=0x7e))
}
#[derive(Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}
impl Audience {
    fn matches(&self, resource: &str) -> bool {
        match self {
            Self::One(value) => value == resource,
            Self::Many(values) => {
                (1..=16).contains(&values.len()) && values.iter().any(|s| s == resource)
            }
        }
    }
}
fn atom(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && !value.chars().any(char::is_control)
        && !value.contains('"')
        && !value.contains('\\')
}
fn url_ok(value: &str) -> bool {
    value.len() <= 2048
        && atom(value, 2048)
        && url::Url::parse(value).is_ok_and(|url| {
            url.username().is_empty()
                && url.password().is_none()
                && url.fragment().is_none()
                && url.query().is_none()
                && (url.scheme() == "https"
                    || (url.scheme() == "http"
                        && url
                            .host_str()
                            .and_then(|host| {
                                host.trim_matches(['[', ']'])
                                    .parse::<std::net::IpAddr>()
                                    .ok()
                            })
                            .is_some_and(|ip| ip.is_loopback())))
        })
}
