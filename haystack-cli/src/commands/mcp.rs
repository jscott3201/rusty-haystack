#[derive(clap::Subcommand)]
pub enum Action {
    /// Serve one bounded MCP session over Unix pipe stdin/stdout
    Stdio {
        /// Version 1 TOML containing the dataset, principal and entity scope
        #[arg(long, value_name = "FILE")]
        config: std::path::PathBuf,
    },
    /// Serve authenticated, bounded MCP over a local loopback HTTP listener
    Http {
        /// Version 1 TOML containing dataset, listener, issuer, keys and subjects
        #[arg(long, value_name = "FILE")]
        config: std::path::PathBuf,
    },
}
pub fn run(action: Action) {
    let result = match action {
        Action::Stdio { config } => {
            #[cfg(feature = "mcp")]
            {
                selected::run(&config)
            }
            #[cfg(not(feature = "mcp"))]
            {
                let _ = config;
                Err::<(), String>(
                    "MCP support is not installed; rebuild rusty-haystack-cli with --features mcp"
                        .into(),
                )
            }
        }
        Action::Http { config } => {
            #[cfg(feature = "mcp-http")]
            {
                http::run(&config)
            }
            #[cfg(not(feature = "mcp-http"))]
            {
                let _ = config;
                Err::<(),String>("MCP HTTP support is not installed; rebuild rusty-haystack-cli with --features mcp-http".into())
            }
        }
    };
    if let Err(error) = result {
        eprintln!("MCP error: {error}");
        std::process::exit(1);
    }
}

#[cfg(feature = "mcp-http")]
mod http;

#[cfg(any(feature = "mcp", feature = "mcp-http"))]
mod selected {
    mod history;
    mod semantics;
    use haystack_app::*;
    #[cfg(feature = "mcp")]
    use haystack_core::graph::SharedGraph;
    use haystack_core::{
        graph::EntityGraph,
        kinds::{Kind, NominalScalar},
        xeto::catalog::{ActivatedCatalog, Catalog},
    };
    pub(super) use history::select_history;
    pub(super) use semantics::{
        SemanticProfile, SemanticScope, semantic_namespace, semantic_scope,
    };
    #[cfg(feature = "mcp")]
    use serde::Deserialize;
    use std::{collections::BTreeSet, io::Read, path::Path, sync::Arc};

    #[cfg(feature = "mcp")]
    use std::{path::PathBuf, time::Duration};
    #[cfg(feature = "mcp")]
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
        h4_libraries: Vec<String>,
        #[serde(default)]
        h4_definitions: Vec<String>,
        #[serde(default)]
        nominal_specs: Vec<String>,
        #[serde(default)]
        history_file: Option<std::path::PathBuf>,
        #[serde(default)]
        history_read: bool,
        principal: String,
        entity_ids: Vec<String>,
        #[serde(default)]
        hidden_tags: Vec<String>,
        #[serde(default = "timeout")]
        request_timeout_ms: u64,
        #[serde(default)]
        max_response_bytes: Option<usize>,
    }
    #[cfg(feature = "mcp")]
    fn timeout() -> u64 {
        10_000
    }
    pub(super) fn response_bytes(selected: Option<usize>, history: bool) -> usize {
        selected.unwrap_or(if history {
            haystack_mcp::HISTORY_RESPONSE_BYTES
        } else {
            1024 * 1024
        })
    }
    #[derive(Clone)]
    pub(super) struct Scope {
        pub(super) principal: Principal,
        pub(super) ids: BTreeSet<String>,
        pub(super) hidden: BTreeSet<String>,
        pub(super) types: Option<Arc<NominalScope>>,
        pub(super) history: bool,
        pub(super) semantics: Option<Arc<SemanticScope>>,
    }
    pub(super) struct NominalScope {
        names: BTreeSet<String>,
        observation: Arc<ActivatedCatalog>,
    }
    pub(super) fn initialize_profile(
        graph: &mut EntityGraph,
        profile: haystack_mcp::OutputProfile,
    ) -> Result<Option<Arc<ActivatedCatalog>>, String> {
        if profile == haystack_mcp::OutputProfile::H4 {
            return Ok(None);
        }
        let catalog =
            Catalog::load_http_pinned().map_err(|_| "MCP typed catalog is unavailable")?;
        let base = graph.namespace_arc().cloned();
        let observation = Arc::new(
            ActivatedCatalog::new(catalog, base.as_deref())
                .map_err(|_| "MCP typed context is unavailable")?,
        );
        graph
            .compare_initialize_catalog(graph.state(), base.as_ref(), observation.clone())
            .map_err(|_| "MCP typed context changed during launch")?;
        Ok(Some(observation))
    }
    pub(super) fn nominal_scope(
        observation: Option<&Arc<ActivatedCatalog>>,
        names: &[String],
    ) -> Result<Option<Arc<NominalScope>>, String> {
        let Some(observation) = observation else {
            return if names.is_empty() {
                Ok(None)
            } else {
                Err("nominal_specs requires the Jeto output profile".into())
            };
        };
        let context = &observation
            .callable("sys.api::readById")
            .ok_or("MCP typed context is unavailable")?
            .context;
        if names.len() > 256
            || names
                .iter()
                .any(|name| name.len() > 256 || !context.is_nominal(name))
        {
            return Err("nominal_specs must name admitted nominal scalar declarations".into());
        }
        let unique = names.iter().cloned().collect::<BTreeSet<_>>();
        if unique.len() != names.len() {
            return Err("duplicate nominal_specs entry".into());
        }
        Ok(Some(Arc::new(NominalScope {
            names: unique,
            observation: observation.clone(),
        })))
    }
    impl ReadPolicy for Scope {
        fn snapshot(&self, principal: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
            if *principal != self.principal {
                return Err(ReadError::Forbidden);
            }
            Ok(Arc::new(self.clone()))
        }
    }
    impl PolicySnapshot for Scope {
        fn scope_key(&self) -> &str {
            "immutable-launch-scope-v1"
        }
        fn function(&self, _: &FunctionIdentity) -> bool {
            false
        }
        fn operation(&self, operation: ReadOperation) -> bool {
            operation == ReadOperation::Read
                || (self.history && operation == ReadOperation::History)
                || (self.semantics.is_some()
                    && matches!(
                        operation,
                        ReadOperation::Definitions | ReadOperation::Libraries
                    ))
        }
        fn entity(&self, id: &str) -> bool {
            self.ids.contains(id)
        }
        fn tag(&self, _: &str, tag: &str) -> bool {
            !self.hidden.contains(tag)
        }
        fn reference(&self, target: &str) -> bool {
            self.ids.contains(target)
        }
        fn reference_display(&self, target: &str) -> bool {
            self.ids.contains(target)
        }
        fn catalog(&self, kind: CatalogKind, name: &str) -> bool {
            self.semantics
                .as_ref()
                .is_some_and(|scope| scope.allows(kind, name))
                || self.types.as_ref().is_some_and(|scope| match kind {
                    CatalogKind::Spec => scope.names.contains(name),
                    CatalogKind::Library => scope.names.iter().any(|qname| {
                        qname
                            .split_once("::")
                            .is_some_and(|(library, _)| library == name)
                    }),
                    _ => false,
                })
        }
        fn semantic_resource(&self, kind: CatalogKind, name: &str) -> bool {
            self.semantics
                .as_ref()
                .is_some_and(|scope| scope.allows(kind, name))
        }
        fn nominal_provenance(&self, value: &NominalScalar) -> bool {
            self.types.as_ref().is_some_and(|scope| {
                let origin = scope.observation.catalog().provenance();
                scope.names.contains(value.spec())
                    && value.catalog() == origin.repository
                    && value.revision() == origin.commit
            })
        }
    }
    pub(super) fn bounded(path: &Path, max: usize) -> Result<String, String> {
        if !std::fs::metadata(path)
            .map_err(|_| "configuration or dataset is unreadable")?
            .is_file()
        {
            return Err("configuration and dataset must be regular files".into());
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // A path replaced with a FIFO between metadata and open must not
            // block before descriptor validation. Regular files ignore this.
            options.custom_flags(libc::O_NONBLOCK);
        }
        let file = options
            .open(path)
            .map_err(|_| "configuration or dataset is unreadable")?;
        if !file
            .metadata()
            .map_err(|_| "configuration or dataset is unreadable")?
            .is_file()
        {
            return Err("configuration and dataset must be regular files".into());
        }
        let mut bytes = Vec::new();
        file.take(max as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "configuration or dataset is unreadable")?;
        if bytes.len() > max {
            return Err("configuration or dataset exceeds its byte limit".into());
        }
        String::from_utf8(bytes).map_err(|_| "configuration or dataset is not UTF-8".into())
    }
    #[cfg(feature = "mcp")]
    pub fn run(path: &Path) -> Result<(), String> {
        let config: Config =
            toml::from_str(&bounded(path, 65_536)?).map_err(|_| "invalid MCP configuration")?;
        let max_response_bytes =
            response_bytes(config.max_response_bytes, config.history_file.is_some());
        if config.version != 1
            || (config.history_read && config.history_file.is_none())
            || config.principal.is_empty()
            || config.principal.len() > 256
            || config.entity_ids.len() > 512
            || config.entity_ids.is_empty()
            || config
                .entity_ids
                .iter()
                .any(|id| id.is_empty() || id.len() > 256)
            || config.hidden_tags.len() > 512
            || config
                .hidden_tags
                .iter()
                .any(|tag| tag.is_empty() || tag.len() > 256)
            || !(10..=60_000).contains(&config.request_timeout_ms)
            || !(config.output_profile.resource_min_response_bytes(
                config.history_file.is_some(),
                config.semantic_resources.is_some(),
            )..=16 * 1024 * 1024)
                .contains(&max_response_bytes)
        {
            return Err("invalid MCP configuration".into());
        }
        let namespace = semantic_namespace(config.semantic_resources)?;
        let semantics = semantic_scope(
            namespace.as_ref(),
            &config.h4_libraries,
            &config.h4_definitions,
        )?;
        let mut graph = load_graph(path, &config.dataset_file, &config.dataset_format)?;
        if let Some(namespace) = namespace {
            graph.set_namespace(namespace);
        }
        let observation = initialize_profile(&mut graph, config.output_profile)?;
        let principal = Principal::TrustedEmbedding {
            subject: config.principal,
        };
        let scope = Arc::new(Scope {
            principal: principal.clone(),
            ids: config.entity_ids.into_iter().collect(),
            hidden: config.hidden_tags.into_iter().collect(),
            types: nominal_scope(observation.as_ref(), &config.nominal_specs)?,
            history: config.history_read,
            semantics,
        });
        let limits = haystack_mcp::Limits {
            request_timeout: Duration::from_millis(config.request_timeout_ms),
            max_response_bytes,
            ..Default::default()
        };
        let builder =
            ApplicationBuilder::new(SharedGraph::new(graph), scope, ReadLimits::default())
                .map_err(|_| "invalid MCP application configuration")?;
        let builder = select_history(builder, path, config.history_file.as_deref())?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(8)
            .enable_all()
            .build()
            .map_err(|_| "cannot create MCP runtime")?;
        let result = runtime.block_on(session(
            builder,
            principal,
            limits,
            config.output_profile,
            config.semantic_resources.is_some(),
        ));
        drop(runtime);
        result
    }
    pub(super) fn load_graph(
        path: &Path,
        dataset_file: &Path,
        dataset_format: &str,
    ) -> Result<EntityGraph, String> {
        let mime = match dataset_format {
            "zinc" => "text/zinc",
            "trio" => "text/trio",
            "json" => "application/json",
            "typed-payload-v1" => "typed-payload-v1",
            _ => return Err("unsupported MCP dataset format".into()),
        };
        let file = if dataset_file.is_absolute() {
            dataset_file.to_path_buf()
        } else {
            path.parent().unwrap_or(Path::new(".")).join(dataset_file)
        };
        let content = bounded(&file, 4 * 1024 * 1024)?;
        // Conservative pre-parser container bound also counts delimiters in
        // strings. A launch file can choose a different spelling if necessary.
        let mut depth = 0usize;
        for byte in content.bytes() {
            match byte {
                b'[' | b'{' | b'<' => {
                    depth += 1;
                    if depth > 64 {
                        return Err("dataset nesting exceeds its limit".into());
                    }
                }
                b']' | b'}' | b'>' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        let grid = if mime == "typed-payload-v1" {
            let value = haystack_core::codecs::typed::decode_with_limits(
                content.as_bytes(),
                haystack_core::codecs::typed::PayloadLimits {
                    max_bytes: 4 * 1024 * 1024,
                    max_depth: 64,
                    max_nodes: 100_000,
                },
            )
            .map_err(|_| "invalid MCP typed-payload-v1 dataset")?;
            let Kind::Grid(grid) = value else {
                return Err("MCP typed-payload-v1 dataset must contain a Grid".into());
            };
            *grid
        } else {
            haystack_core::codecs::codec_for(mime)
                .expect("selected codec")
                .decode_grid(&content)
                .map_err(|_| "invalid MCP dataset")?
        };
        if grid.rows.len() > 10_000 {
            return Err("dataset row count exceeds its limit".into());
        }
        let graph = EntityGraph::from_grid(&grid, None).map_err(|_| "invalid MCP entity graph")?;
        Ok(graph)
    }
    #[cfg(all(feature = "mcp", unix))]
    async fn session(
        builder: ApplicationBuilder,
        principal: Principal,
        limits: haystack_mcp::Limits,
        profile: haystack_mcp::OutputProfile,
        semantic_resources: bool,
    ) -> Result<(), String> {
        use std::os::fd::AsFd;
        use tokio::{
            net::unix::pipe::{Receiver, Sender},
            signal::unix::{SignalKind, signal},
        };
        let input = std::io::stdin()
            .as_fd()
            .try_clone_to_owned()
            .and_then(Receiver::from_owned_fd)
            .map_err(|_| "MCP stdio requires Unix pipe stdin")?;
        let output = std::io::stdout()
            .as_fd()
            .try_clone_to_owned()
            .and_then(Sender::from_owned_fd)
            .map_err(|_| "MCP stdio requires Unix pipe stdout")?;
        let mut terminate =
            signal(SignalKind::terminate()).map_err(|_| "cannot register termination signal")?;
        let mut interrupt =
            signal(SignalKind::interrupt()).map_err(|_| "cannot register interrupt signal")?;
        let owner = builder
            .start(&tokio::runtime::Handle::current())
            .map_err(|_| "MCP application startup failed")?;
        if owner.ready().await.is_err() {
            owner.terminated().await;
            return Err("MCP application startup failed".into());
        }
        let stop = CancellationToken::new();
        let serving = haystack_mcp::serve(
            owner.handle(),
            principal,
            input,
            output,
            haystack_mcp::StdioConfig {
                limits,
                output_profile: profile,
                semantic_resources,
            },
            stop.clone(),
        );
        tokio::pin!(serving);
        let result = tokio::select! {
            result = &mut serving => result,
            _ = terminate.recv() => { stop.cancel(); serving.await },
            _ = interrupt.recv() => { stop.cancel(); serving.await },
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
        eprintln!("MCP session terminated; all admitted work released");
        Ok(())
    }
    #[cfg(all(feature = "mcp", not(unix)))]
    async fn session(
        _: ApplicationBuilder,
        _: Principal,
        _: haystack_mcp::Limits,
        _: haystack_mcp::OutputProfile,
        _: bool,
    ) -> Result<(), String> {
        Err("MCP stdio requires the Unix pipe platform profile".into())
    }
}
