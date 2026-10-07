//! Bounded SCRAM SHA-256 for the first-party server's two-request profile.
//!
//! Client-first data accompanies HELLO and outer data uses padded standard
//! base64. This is not the published three-request Haystack/base64url exchange.
use crate::{
    config::validate_http_url,
    error::{ClientError, http_error},
};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use haystack_core::auth;
use reqwest::{
    Client,
    header::{HeaderMap, HeaderValue},
};
use std::{collections::HashMap, time::Duration};
use zeroize::Zeroizing;

const MAX_HEADERS: usize = 8192;
const MAX_DECODED: usize = 4096;
// A timed-out derivation may finish in the blocking pool. Its permit stays with
// that task, so cancellation cannot create an unbounded tail of expensive work.
static DERIVATIONS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

fn failed(message: &str) -> ClientError {
    ClientError::AuthFailed(message.into())
}

pub(crate) fn sensitive_header(value: &str) -> Result<HeaderValue, ClientError> {
    let mut header =
        HeaderValue::from_str(value).map_err(|_| failed("invalid authentication header"))?;
    header.set_sensitive(true);
    Ok(header)
}

/// Authenticate using a 30-second total budget without retaining credentials for refresh.
///
/// The supplied client's TLS, redirect, retry, and per-request timeout policies
/// are caller-owned. Prefer `HaystackClient::connect_with_config` for safe defaults.
pub async fn authenticate(
    client: &Client,
    base_url: &str,
    username: &str,
    password: &str,
) -> Result<String, ClientError> {
    authenticate_with_timeout(
        client,
        base_url,
        username,
        password,
        Duration::from_secs(30),
    )
    .await
}

/// Authenticate with one total deadline across both HTTP phases and derivation.
/// At most two bounded PBKDF2 computations run concurrently. After cancellation,
/// an already-running computation finishes without sending another request.
pub async fn authenticate_with_timeout(
    client: &Client,
    base_url: &str,
    username: &str,
    password: &str,
    budget: Duration,
) -> Result<String, ClientError> {
    validate_http_url(base_url)?;
    let deadline = tokio::time::Instant::now()
        .checked_add(budget)
        .ok_or_else(|| {
            ClientError::Connection("authentication timeout exceeds supported range".into())
        })?;
    tokio::time::timeout_at(
        deadline,
        handshake(client, base_url, username, password, deadline, budget),
    )
    .await
    .map_err(|_| ClientError::Timeout(budget))?
}

async fn handshake(
    client: &Client,
    base_url: &str,
    username: &str,
    password: &str,
    deadline: tokio::time::Instant,
    budget: Duration,
) -> Result<String, ClientError> {
    if username.len() > 1024 {
        return Err(failed("username exceeds authentication limit"));
    }
    let about_url = format!("{}/about", base_url.trim_end_matches('/'));
    let (nonce, first) = auth::client_first_message(username);
    let hello = Zeroizing::new(format!(
        "HELLO username={}, data={first}",
        BASE64.encode(username)
    ));
    if tokio::time::Instant::now() >= deadline {
        return Err(ClientError::Timeout(budget));
    }
    let response = client
        .get(&about_url)
        .header("Authorization", sensitive_header(&hello)?)
        .send()
        .await
        .map_err(http_error)?;
    if response.status() != reqwest::StatusCode::UNAUTHORIZED {
        return Err(failed("expected 401 SCRAM challenge"));
    }
    let challenge = scram_challenge(response.headers())?;
    let fields = parameters(&challenge)?;
    let token = required(&fields, "handshakeToken")?.to_string();
    if required(&fields, "hash")? != "SHA-256" {
        return Err(failed("unsupported SCRAM hash (requires SHA-256)"));
    }
    let first = required(&fields, "data")?.to_string();
    validate_server_first(&first, &nonce)?;
    drop(response);

    let permit = DERIVATIONS
        .acquire()
        .await
        .map_err(|_| failed("SCRAM worker unavailable"))?;
    let password = Zeroizing::new(password.to_string());
    let username = username.to_string();
    let (final_message, expected_signature) = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        auth::client_final_message(&password, &nonce, &first, &username)
    })
    .await
    .map_err(|_| failed("SCRAM derivation failed"))?
    .map_err(|_| failed("invalid SCRAM challenge"))?;
    let final_message = Zeroizing::new(final_message);
    let proof = Zeroizing::new(format!(
        "SCRAM handshakeToken={token}, data={}",
        *final_message
    ));
    if tokio::time::Instant::now() >= deadline {
        return Err(ClientError::Timeout(budget));
    }
    let response = client
        .get(&about_url)
        .header("Authorization", sensitive_header(&proof)?)
        .send()
        .await
        .map_err(http_error)?;
    if !response.status().is_success() {
        return Err(failed("SCRAM credentials rejected"));
    }
    let info = bounded_headers(response.headers(), "authentication-info")?;
    if info.len() != 1 {
        return Err(failed("expected one Authentication-Info header"));
    }
    let fields = parameters(info[0])?;
    if fields.get("hash").is_some_and(|hash| *hash != "SHA-256") {
        return Err(failed("unsupported SCRAM final hash (requires SHA-256)"));
    }
    let token = required(&fields, "authToken")?;
    let final_data = decoded(required(&fields, "data")?)?;
    let final_text =
        std::str::from_utf8(&final_data).map_err(|_| failed("invalid SCRAM final data"))?;
    let fields = parameters(final_text)?;
    if fields.len() != 1 || !final_text.starts_with("v=") {
        return Err(failed("invalid SCRAM final fields"));
    }
    let received_signature = decoded(required(&fields, "v")?)?;
    if received_signature.len() != 32 || received_signature != expected_signature {
        return Err(failed("server signature verification failed"));
    }
    Ok(token.to_string())
}

fn bounded_headers<'a>(headers: &'a HeaderMap, name: &str) -> Result<Vec<&'a str>, ClientError> {
    let mut total = 0;
    let mut result = Vec::new();
    for value in headers.get_all(name) {
        total += value.as_bytes().len();
        if total > MAX_HEADERS || result.len() == 16 {
            return Err(failed("authentication headers exceed limit"));
        }
        result.push(
            value
                .to_str()
                .map_err(|_| failed("invalid authentication header encoding"))?,
        );
    }
    if result.is_empty() {
        return Err(failed("missing authentication header"));
    }
    Ok(result)
}

fn scram_challenge(headers: &HeaderMap) -> Result<String, ClientError> {
    let mut result = None;
    for header in bounded_headers(headers, "www-authenticate")? {
        let mut selected = false;
        for part in challenge_parts(header)?.into_iter().map(str::trim) {
            // RFC 9110 permits a challenge containing only an auth-scheme.
            // A parameter with whitespace before '=' is still a parameter.
            let (scheme, rest) = part.split_once(char::is_whitespace).unwrap_or((part, ""));
            if is_auth_scheme_token(scheme) && !rest.trim_start().starts_with('=') {
                selected = scheme.eq_ignore_ascii_case("SCRAM");
                if selected {
                    if result.is_some() {
                        return Err(failed("multiple SCRAM challenges"));
                    }
                    result = Some(rest.to_string());
                }
            } else if selected && let Some(result) = &mut result {
                result.push(',');
                result.push_str(part);
            }
        }
    }
    result.ok_or_else(|| failed("no supported SCRAM challenge"))
}

fn is_auth_scheme_token(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

// Other advertised schemes may use quoted realms containing commas. Keep
// their contents opaque while finding complete SCRAM challenges.
fn challenge_parts(input: &str) -> Result<Vec<&str>, ClientError> {
    let mut parts = Vec::new();
    let (mut quoted, mut escaped, mut start) = (false, false, 0);
    for (index, byte) in input.bytes().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        match byte {
            b'\\' if quoted => escaped = true,
            b'"' => quoted = !quoted,
            b',' if !quoted => {
                parts.push(&input[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    if quoted || escaped {
        return Err(failed("malformed authentication quoting"));
    }
    parts.push(&input[start..]);
    Ok(parts)
}

fn parameters(input: &str) -> Result<HashMap<String, &str>, ClientError> {
    let mut fields = HashMap::new();
    for part in input.split(',') {
        let (key, value) = part
            .trim()
            .split_once('=')
            .ok_or_else(|| failed("malformed authentication fields"))?;
        let key = key.trim();
        let value = value.trim();
        if key.is_empty()
            || value.is_empty()
            || !key.bytes().all(|b| b.is_ascii_alphanumeric())
            || !value
                .bytes()
                .all(|b| b.is_ascii_graphic() && !matches!(b, b'"' | b'\\'))
            || fields.insert(key.to_ascii_lowercase(), value).is_some()
        {
            return Err(failed("empty, duplicate, or invalid authentication field"));
        }
    }
    Ok(fields)
}
fn required<'a>(fields: &HashMap<String, &'a str>, name: &str) -> Result<&'a str, ClientError> {
    fields
        .get(&name.to_ascii_lowercase())
        .copied()
        .ok_or_else(|| failed("missing required authentication field"))
}
fn decoded(input: &str) -> Result<Vec<u8>, ClientError> {
    if input.len() > MAX_DECODED * 4 / 3 + 4 {
        return Err(failed("SCRAM data exceeds limit"));
    }
    let value = BASE64
        .decode(input)
        .map_err(|_| failed("invalid SCRAM base64"))?;
    if value.len() > MAX_DECODED {
        return Err(failed("decoded SCRAM data exceeds limit"));
    }
    Ok(value)
}
fn validate_server_first(encoded: &str, client_nonce: &str) -> Result<(), ClientError> {
    let bytes = decoded(encoded)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| failed("invalid SCRAM data encoding"))?;
    let fields = parameters(text)?;
    // The existing core crypto contract consumes r,s,i in this order. Reject
    // extensions, duplicate fields, empty salts and zero work before calling it.
    if fields.len() != 3
        || !text.starts_with("r=")
        || !text.split(',').nth(1).is_some_and(|s| s.starts_with("s="))
        || !text.split(',').nth(2).is_some_and(|s| s.starts_with("i="))
    {
        return Err(failed("unsupported SCRAM server-first fields"));
    }
    let nonce = required(&fields, "r")?;
    if !nonce.starts_with(client_nonce) || nonce.len() <= client_nonce.len() || nonce.len() > 1024 {
        return Err(failed("invalid SCRAM server nonce"));
    }
    let salt = decoded(required(&fields, "s")?)?;
    if salt.is_empty() || salt.len() > 1024 {
        return Err(failed("invalid SCRAM salt length"));
    }
    let iterations = required(&fields, "i")?
        .parse::<u32>()
        .map_err(|_| failed("invalid SCRAM iteration count"))?;
    if !(1..=auth::MAX_CLIENT_ITERATIONS).contains(&iterations) {
        return Err(failed("SCRAM iteration count exceeds bounds"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn impossible_or_zero_handshake_budgets_fail_before_network() {
        let client = crate::ClientConfig::default()
            .build_reqwest_client()
            .unwrap();
        assert!(matches!(
            authenticate_with_timeout(
                &client,
                "http://localhost:1/api",
                "user",
                "password",
                Duration::MAX
            )
            .await,
            Err(ClientError::Connection(_))
        ));
        assert!(
            matches!(authenticate_with_timeout(&client, "http://localhost:1/api", "user", "password", Duration::ZERO).await, Err(ClientError::Timeout(duration)) if duration.is_zero())
        );
    }

    #[test]
    fn handshake_headers_are_sensitive() {
        let header = sensitive_header("SCRAM handshakeToken=private-sentinel, data=proof").unwrap();
        assert!(header.is_sensitive());
        assert!(!format!("{header:?}").contains("private-sentinel"));
        assert!(matches!(
            sensitive_header("secret\r\nheader"),
            Err(ClientError::AuthFailed(_))
        ));
    }

    #[test]
    fn scheme_selection_and_http_parameter_names_follow_header_syntax() {
        let mut headers = HeaderMap::new();
        headers.insert("www-authenticate", HeaderValue::from_static("Basic realm=\"ignored, SCRAM fake\", SCRAM HandshakeToken = token, Hash = SHA-256, Data = YQ=="));
        let selected = scram_challenge(&headers).unwrap();
        let fields = parameters(&selected).unwrap();
        assert_eq!(required(&fields, "handshakeToken").unwrap(), "token");
        assert_eq!(required(&fields, "hash").unwrap(), "SHA-256");
        assert!(parameters("hash=SHA-256, HASH=SHA-512").is_err());
        assert!(parameters("hash=\"SHA-256\"").is_err());
    }

    #[test]
    fn bare_alternative_schemes_bound_scram_parameters() {
        let scram = "handshakeToken=token, hash=SHA-256, data=YQ==";
        for header in [
            format!("SCRAM {scram}, Negotiate"),
            format!("Negotiate, SCRAM {scram}"),
            format!("SCRAM {scram}, X-Auth, realm=opaque"),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("www-authenticate", HeaderValue::from_str(&header).unwrap());
            let selected = scram_challenge(&headers).unwrap();
            let fields = parameters(&selected)
                .expect("alternative scheme must not become a SCRAM parameter");
            assert_eq!(fields.len(), 3);
            assert_eq!(required(&fields, "handshakeToken").unwrap(), "token");
            assert_eq!(required(&fields, "hash").unwrap(), "SHA-256");
            assert_eq!(required(&fields, "data").unwrap(), "YQ==");
        }
    }

    #[test]
    fn invalid_bare_segments_are_not_silently_discarded() {
        for suffix in ["Invalid/Scheme", "Invalid@Scheme", ""] {
            let mut headers = HeaderMap::new();
            let header = format!("SCRAM handshakeToken=token, hash=SHA-256, data=YQ==, {suffix}");
            headers.insert("www-authenticate", HeaderValue::from_str(&header).unwrap());
            let selected = scram_challenge(&headers).unwrap();
            assert!(parameters(&selected).is_err());
        }
    }

    #[test]
    fn bare_scram_challenge_still_counts_as_a_duplicate() {
        let scram = "handshakeToken=token, hash=SHA-256, data=YQ==";
        for header in [
            format!("SCRAM {scram}, SCRAM"),
            format!("SCRAM, SCRAM {scram}"),
            format!("SCRAM {scram}, Negotiate, scram"),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("www-authenticate", HeaderValue::from_str(&header).unwrap());
            assert!(
                matches!(scram_challenge(&headers), Err(ClientError::AuthFailed(message)) if message == "multiple SCRAM challenges")
            );
        }
    }

    #[test]
    fn challenge_limits_apply_before_base64_allocation() {
        assert!(decoded(&"A".repeat(MAX_DECODED * 2)).is_err());
        assert!(decoded(&BASE64.encode(vec![0; MAX_DECODED + 1])).is_err());
        assert_eq!(
            decoded(&BASE64.encode(vec![0; MAX_DECODED])).unwrap().len(),
            MAX_DECODED
        );
    }
}
