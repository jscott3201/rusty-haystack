use super::selected::{
    Scope, SemanticProfile, bounded, initialize_profile, load_graph, nominal_scope, response_bytes,
    select_history, semantic_namespace, semantic_scope,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use haystack_app::*;
use haystack_core::graph::SharedGraph;
use haystack_mcp::{HttpConfig, JwtConfig, JwtVerifier, VerificationKey};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    version: u32,
    dataset_file: PathBuf,
    dataset_format: String,
    #[serde(default)]
    output_profile: haystack_mcp::OutputProfile,
    #[serde(default)]
    semantic_resources: Option<SemanticProfile>,
    #[serde(default)]
    history_file: Option<PathBuf>,
    listen: SocketAddr,
    issuer: String,
    required_scope: String,
    keys: Vec<Key>,
    subjects: Vec<Subject>,
    #[serde(default)]
    revoked_jti: Vec<String>,
    #[serde(default)]
    allowed_origins: Vec<String>,
    #[serde(default = "timeout")]
    request_timeout_ms: u64,
    #[serde(default)]
    max_response_bytes: Option<usize>,
    #[serde(default = "connections")]
    max_connections: usize,
    #[serde(default = "in_flight")]
    max_in_flight: usize,
}
fn timeout() -> u64 {
    10_000
}
fn connections() -> usize {
    32
}
fn in_flight() -> usize {
    16
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Key {
    kid: String,
    algorithm: String,
    public_key_der_base64: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Subject {
    subject: String,
    entity_ids: Vec<String>,
    #[serde(default)]
    hidden_tags: Vec<String>,
    #[serde(default)]
    nominal_specs: Vec<String>,
    #[serde(default)]
    history_read: bool,
    #[serde(default)]
    h4_libraries: Vec<String>,
    #[serde(default)]
    h4_definitions: Vec<String>,
}
struct Policy(BTreeMap<String, Arc<Scope>>);
impl ReadPolicy for Policy {
    fn snapshot(&self, principal: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
        let Principal::Authenticated { subject, .. } = principal else {
            return Err(ReadError::Forbidden);
        };
        let scope = self
            .0
            .get(subject)
            .filter(|scope| scope.principal == *principal)
            .ok_or(ReadError::Forbidden)?;
        Ok(scope.clone())
    }
}
pub(super) fn run(path: &Path) -> Result<(), String> {
    let config: Config =
        toml::from_str(&bounded(path, 65_536)?).map_err(|_| "invalid MCP HTTP configuration")?;
    let max_response_bytes =
        response_bytes(config.max_response_bytes, config.history_file.is_some());
    if config.version != 1
        || (config.history_file.is_none() && config.subjects.iter().any(|s| s.history_read))
        || !config.listen.ip().is_loopback()
        || !(1..=512).contains(&config.subjects.len())
        || !(1..=16).contains(&config.keys.len())
        || !(10..=60_000).contains(&config.request_timeout_ms)
        || !(config.output_profile.resource_min_response_bytes(
            config.history_file.is_some(),
            config.semantic_resources.is_some(),
        )..=16 * 1024 * 1024)
            .contains(&max_response_bytes)
        || !(1..=128).contains(&config.max_connections)
        || !(1..=128).contains(&config.max_in_flight)
    {
        return Err("invalid MCP HTTP configuration".into());
    }
    let namespace = semantic_namespace(config.semantic_resources)?;
    let semantic_scopes = config
        .subjects
        .iter()
        .map(|subject| {
            semantic_scope(
                namespace.as_ref(),
                &subject.h4_libraries,
                &subject.h4_definitions,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut graph = load_graph(path, &config.dataset_file, &config.dataset_format)?;
    if let Some(namespace) = namespace {
        graph.set_namespace(namespace);
    }
    let observation = initialize_profile(&mut graph, config.output_profile)?;
    let mut scopes = BTreeMap::new();
    for (subject, semantics) in config.subjects.iter().zip(semantic_scopes) {
        if subject.subject.is_empty()
            || subject.subject.len() > 256
            || subject.entity_ids.is_empty()
            || subject.entity_ids.len() > 512
            || subject.hidden_tags.len() > 512
            || subject
                .entity_ids
                .iter()
                .chain(&subject.hidden_tags)
                .any(|s| s.is_empty() || s.len() > 256)
        {
            return Err("invalid MCP HTTP subject scope".into());
        }
        let scope = Arc::new(Scope {
            principal: Principal::authenticated(
                &subject.subject,
                vec![config.required_scope.clone()],
            ),
            ids: subject.entity_ids.iter().cloned().collect(),
            hidden: subject.hidden_tags.iter().cloned().collect(),
            types: nominal_scope(observation.as_ref(), &subject.nominal_specs)?,
            history: subject.history_read,
            semantics,
        });
        if scopes.insert(subject.subject.clone(), scope).is_some() {
            return Err("duplicate MCP HTTP subject".into());
        }
    }
    let keys = config
        .keys
        .iter()
        .map(|key| {
            if key.algorithm != "RS256" {
                return Err("unsupported MCP HTTP key algorithm");
            }
            Ok(VerificationKey {
                kid: key.kid.clone(),
                public_key_der: STANDARD
                    .decode(&key.public_key_der_base64)
                    .map_err(|_| "invalid MCP HTTP public key")?,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let builder = ApplicationBuilder::new(
        SharedGraph::new(graph),
        Arc::new(Policy(scopes)),
        ReadLimits::default(),
    )
    .map_err(|_| "invalid MCP HTTP application configuration")?;
    let builder = select_history(builder, path, config.history_file.as_deref())?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(8)
        .enable_all()
        .build()
        .map_err(|_| "cannot create MCP runtime")?;
    let result = runtime.block_on(session(config, keys, builder));
    drop(runtime);
    result
}
async fn session(
    config: Config,
    keys: Vec<VerificationKey>,
    builder: ApplicationBuilder,
) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .map_err(|_| "cannot bind MCP HTTP listener")?;
    let mut http = HttpConfig::local(
        listener
            .local_addr()
            .map_err(|_| "cannot read MCP HTTP listener address")?,
    );
    http.allowed_origins = config.allowed_origins;
    http.output_profile = config.output_profile;
    http.semantic_resources = config.semantic_resources.is_some();
    http.max_connections = config.max_connections;
    http.limits.max_in_flight = config.max_in_flight;
    http.limits.request_timeout = Duration::from_millis(config.request_timeout_ms);
    http.limits.max_response_bytes =
        response_bytes(config.max_response_bytes, config.history_file.is_some());
    let verifier = Arc::new(
        JwtVerifier::new(JwtConfig {
            issuer: config.issuer,
            resource: http.resource.clone(),
            required_scope: config.required_scope,
            subjects: config.subjects.into_iter().map(|s| s.subject).collect(),
            keys,
            revoked_jti: config.revoked_jti,
        })
        .map_err(|_| "invalid MCP HTTP verifier configuration")?,
    );
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|_| "cannot register termination signal")?;
    let owner = builder
        .start(&tokio::runtime::Handle::current())
        .map_err(|_| "MCP application startup failed")?;
    if owner.ready().await.is_err() {
        owner.terminated().await;
        return Err("MCP application startup failed".into());
    }
    let stop = CancellationToken::new();
    let resource = http.resource.clone();
    let serving = haystack_mcp::serve_http(owner.handle(), listener, http, verifier, stop.clone());
    tokio::pin!(serving);
    // Poll the adapter once before reporting readiness, so its configuration
    // validation cannot be mistaken for a ready endpoint.
    let result = tokio::select! {
        biased;
        result=&mut serving=>Some(result),
        _=tokio::task::yield_now()=>None,
    };
    let result = if let Some(result) = result {
        result
    } else {
        eprintln!("MCP HTTP ready: {resource}");
        #[cfg(unix)]
        {
            tokio::select! {
                result=&mut serving=>result,
                _=terminate.recv()=>{stop.cancel();serving.await},
                _=tokio::signal::ctrl_c()=>{stop.cancel();serving.await},
            }
        }
        #[cfg(not(unix))]
        {
            tokio::select! {result=&mut serving=>result,_=tokio::signal::ctrl_c()=>{stop.cancel();serving.await}}
        }
    };
    let close = owner.close().await;
    let terminated = owner.terminated().await;
    if close.is_err() || terminated.close.is_err() || !terminated.cleanup_errors.is_empty() {
        return Err("MCP application cleanup failed".into());
    }
    let report = result.map_err(|error| error.to_string())?;
    if report.admitted != report.released || owner.handle().outstanding_tasks() != 0 {
        return Err("MCP work cleanup incomplete".into());
    }
    eprintln!("MCP HTTP terminated; all admitted work released");
    Ok(())
}
