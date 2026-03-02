use std::collections::HashSet;

use crate::error::ClientError;
use crate::transport::Transport;
use crate::transport::http::HttpTransport;
use crate::transport::ws::WsTransport;
use haystack_core::data::{HCol, HDict, HGrid};
use haystack_core::kinds::{HRef, Kind, Number};

/// A client for communicating with a Haystack HTTP API server.
///
/// Provides typed methods for all standard Haystack ops (about, read, hisRead,
/// etc.) as well as a generic `call` method for custom ops.
///
/// Supports both HTTP and WebSocket transports.
pub struct HaystackClient<T: Transport> {
    transport: T,
}

impl HaystackClient<HttpTransport> {
    /// Connect to a Haystack server via HTTP, performing SCRAM authentication.
    ///
    /// # Arguments
    /// * `url` - The server API root (e.g. `http://localhost:8080/api`)
    /// * `username` - The username to authenticate as
    /// * `password` - The user's plaintext password
    pub async fn connect(url: &str, username: &str, password: &str) -> Result<Self, ClientError> {
        crate::ensure_crypto_provider();
        let client = reqwest::Client::new();
        let auth_token = crate::auth::authenticate(&client, url, username, password).await?;
        let transport = HttpTransport::new(url, auth_token);
        Ok(Self { transport })
    }

    /// Connect to a Haystack server via HTTP with mutual TLS (mTLS) client
    /// certificate authentication, then perform SCRAM authentication.
    ///
    /// Builds a custom `reqwest::Client` configured with the provided TLS
    /// identity (client certificate + key) and optional CA certificate, then
    /// runs the standard SCRAM handshake over that client.
    ///
    /// # Arguments
    /// * `url` - The server API root (e.g. `https://localhost:8443/api`)
    /// * `username` - The username to authenticate as
    /// * `password` - The user's plaintext password
    /// * `tls` - The mTLS configuration (cert, key, optional CA)
    pub async fn connect_with_tls(
        url: &str,
        username: &str,
        password: &str,
        tls: &crate::tls::TlsConfig,
    ) -> Result<Self, ClientError> {
        crate::ensure_crypto_provider();
        // Combine cert + key into a single PEM buffer for reqwest::Identity
        let mut combined_pem = tls.client_cert_pem.clone();
        combined_pem.extend_from_slice(&tls.client_key_pem);

        let identity = reqwest::Identity::from_pem(&combined_pem)
            .map_err(|e| ClientError::Connection(format!("invalid client certificate: {e}")))?;

        let mut builder = reqwest::Client::builder().identity(identity);

        if let Some(ref ca) = tls.ca_cert_pem {
            let cert = reqwest::Certificate::from_pem(ca)
                .map_err(|e| ClientError::Connection(format!("invalid CA certificate: {e}")))?;
            builder = builder.add_root_certificate(cert);
        }

        let client = builder
            .build()
            .map_err(|e| ClientError::Connection(format!("TLS client build failed: {e}")))?;

        let auth_token = crate::auth::authenticate(&client, url, username, password).await?;
        let transport = HttpTransport::new(url, auth_token);
        Ok(Self { transport })
    }
}

impl HaystackClient<WsTransport> {
    /// Connect to a Haystack server via WebSocket.
    ///
    /// Performs SCRAM authentication over HTTP first to obtain an auth token,
    /// then establishes a WebSocket connection using that token.
    ///
    /// # Arguments
    /// * `url` - The server API root for HTTP auth (e.g. `http://localhost:8080/api`)
    /// * `ws_url` - The WebSocket URL (e.g. `ws://localhost:8080/api/ws`)
    /// * `username` - The username to authenticate as
    /// * `password` - The user's plaintext password
    pub async fn connect_ws(
        url: &str,
        ws_url: &str,
        username: &str,
        password: &str,
    ) -> Result<Self, ClientError> {
        crate::ensure_crypto_provider();
        // Authenticate via HTTP first to get the token
        let client = reqwest::Client::new();
        let auth_token = crate::auth::authenticate(&client, url, username, password).await?;

        // Connect WebSocket with the token
        let transport = WsTransport::connect(ws_url, &auth_token).await?;
        Ok(Self { transport })
    }
}

impl<T: Transport> HaystackClient<T> {
    /// Create a client with an already-configured transport.
    ///
    /// Useful for testing or when you have a custom [`Transport`] implementation.
    pub fn from_transport(transport: T) -> Self {
        Self { transport }
    }

    /// Call a raw Haystack op with a request grid.
    ///
    /// `op` is the operation name (e.g. `"read"`, `"hisRead"`). The server
    /// returns an error grid (as [`ClientError::Grid`]) if the op fails.
    pub async fn call(&self, op: &str, req: &HGrid) -> Result<HGrid, ClientError> {
        self.transport.call(op, req).await
    }

    // -----------------------------------------------------------------------
    // Standard ops
    // -----------------------------------------------------------------------

    /// Call the `about` op. Returns a single-row grid with server metadata
    /// including `vendorName`, `productName`, `productVersion`, and `tz`.
    pub async fn about(&self) -> Result<HGrid, ClientError> {
        self.call("about", &HGrid::new()).await
    }

    /// Call the `ops` op. Returns a grid listing every operation the server
    /// supports, with `name`, `summary`, and `doc` columns.
    pub async fn ops(&self) -> Result<HGrid, ClientError> {
        self.call("ops", &HGrid::new()).await
    }

    /// Call the `formats` op. Returns a grid of MIME types the server can
    /// read/write, with `mime`, `receive`, and `send` columns.
    pub async fn formats(&self) -> Result<HGrid, ClientError> {
        self.call("formats", &HGrid::new()).await
    }

    /// Call the `libs` op. Returns a grid of Xeto library modules installed
    /// on the server, including `name` and `version` for each library.
    pub async fn libs(&self) -> Result<HGrid, ClientError> {
        self.call("libs", &HGrid::new()).await
    }

    /// Call the `read` op with a Haystack filter expression and optional limit.
    ///
    /// `filter` is a Haystack filter string (e.g. `"site"`, `"equip and siteRef==@s"`,
    /// `"temp > 72"`). Returns a grid of matching entity dicts. Pass `limit` to
    /// cap the number of results. Returns an empty grid if nothing matches.
    pub async fn read(&self, filter: &str, limit: Option<usize>) -> Result<HGrid, ClientError> {
        let mut row = HDict::new();
        row.set("filter", Kind::Str(filter.to_string()));
        if let Some(lim) = limit {
            row.set("limit", Kind::Number(Number::unitless(lim as f64)));
        }
        let cols = if limit.is_some() {
            vec![HCol::new("filter"), HCol::new("limit")]
        } else {
            vec![HCol::new("filter")]
        };
        let grid = HGrid::from_parts(HDict::new(), cols, vec![row]);
        self.call("read", &grid).await
    }

    /// Call the `read` op with a list of entity ref ids.
    ///
    /// Each entry in `ids` is a ref value string (e.g. `"@site-1"`, `"@equip-2"`).
    /// Returns one row per id. Rows for unknown ids contain only a `Null` marker.
    pub async fn read_by_ids(&self, ids: &[&str]) -> Result<HGrid, ClientError> {
        let rows: Vec<HDict> = ids
            .iter()
            .map(|id| {
                let mut d = HDict::new();
                d.set("id", Kind::Ref(HRef::from_val(*id)));
                d
            })
            .collect();
        let grid = HGrid::from_parts(HDict::new(), vec![HCol::new("id")], rows);
        self.call("read", &grid).await
    }

    /// Call the `nav` op to walk the site/equip/point navigation tree.
    ///
    /// Pass `None` to get root-level nodes, or `Some(nav_id)` to expand a
    /// specific node. Returns a grid of child navigation entries.
    pub async fn nav(&self, nav_id: Option<&str>) -> Result<HGrid, ClientError> {
        let mut row = HDict::new();
        if let Some(id) = nav_id {
            row.set("navId", Kind::Str(id.to_string()));
        }
        let grid = HGrid::from_parts(HDict::new(), vec![HCol::new("navId")], vec![row]);
        self.call("nav", &grid).await
    }

    /// Call the `defs` op to query the server's tag/def ontology.
    ///
    /// `filter` is an optional Haystack filter (e.g. `"point"`, `"equip"`) to
    /// narrow results. Returns a grid of matching def dicts.
    pub async fn defs(&self, filter: Option<&str>) -> Result<HGrid, ClientError> {
        let mut row = HDict::new();
        if let Some(f) = filter {
            row.set("filter", Kind::Str(f.to_string()));
        }
        let grid = HGrid::from_parts(HDict::new(), vec![HCol::new("filter")], vec![row]);
        self.call("defs", &grid).await
    }

    /// Call the `watchSub` op to subscribe to real-time changes for a set of entities.
    ///
    /// `ids` are ref value strings (e.g. `["@point-1", "@point-2"]`). `lease` is
    /// an optional duration string (e.g. `"1min"`, `"30sec"`, `"1hr"`) controlling
    /// how long the server keeps the watch alive without polls. Returns a grid
    /// whose meta contains the assigned `watchId`.
    pub async fn watch_sub(&self, ids: &[&str], lease: Option<&str>) -> Result<HGrid, ClientError> {
        let rows: Vec<HDict> = ids
            .iter()
            .map(|id| {
                let mut d = HDict::new();
                d.set("id", Kind::Ref(HRef::from_val(*id)));
                d
            })
            .collect();
        let mut meta = HDict::new();
        if let Some(l) = lease {
            meta.set("lease", Kind::Str(l.to_string()));
        }
        let grid = HGrid::from_parts(meta, vec![HCol::new("id")], rows);
        self.call("watchSub", &grid).await
    }

    /// Call the `watchPoll` op to poll a watch for changed entity values.
    ///
    /// `watch_id` is the identifier returned by [`watch_sub`](Self::watch_sub).
    /// Returns a grid of entities whose `curVal` has changed since the last poll.
    pub async fn watch_poll(&self, watch_id: &str) -> Result<HGrid, ClientError> {
        let mut meta = HDict::new();
        meta.set("watchId", Kind::Str(watch_id.to_string()));
        let grid = HGrid::from_parts(meta, vec![], vec![]);
        self.call("watchPoll", &grid).await
    }

    /// Call the `watchUnsub` op to remove entities from an active watch.
    ///
    /// `watch_id` identifies the watch. `ids` are the ref value strings to
    /// unsubscribe (e.g. `["@point-1"]`). The watch is closed automatically
    /// when all entities have been removed.
    pub async fn watch_unsub(&self, watch_id: &str, ids: &[&str]) -> Result<HGrid, ClientError> {
        let rows: Vec<HDict> = ids
            .iter()
            .map(|id| {
                let mut d = HDict::new();
                d.set("id", Kind::Ref(HRef::from_val(*id)));
                d
            })
            .collect();
        let mut meta = HDict::new();
        meta.set("watchId", Kind::Str(watch_id.to_string()));
        let grid = HGrid::from_parts(meta, vec![HCol::new("id")], rows);
        self.call("watchUnsub", &grid).await
    }

    /// Call the `pointWrite` op to write a value to a writable point.
    ///
    /// `id` is the point ref (e.g. `"@point-1"`). `level` is the priority
    /// array level 1–17 per the Haystack spec (1=emergency, 8=manual,
    /// 16=default, 17=fallback). `val` is the value to write at that level.
    pub async fn point_write(&self, id: &str, level: u8, val: Kind) -> Result<HGrid, ClientError> {
        let mut row = HDict::new();
        row.set("id", Kind::Ref(HRef::from_val(id)));
        row.set("level", Kind::Number(Number::unitless(level as f64)));
        row.set("val", val);
        let grid = HGrid::from_parts(
            HDict::new(),
            vec![HCol::new("id"), HCol::new("level"), HCol::new("val")],
            vec![row],
        );
        self.call("pointWrite", &grid).await
    }

    /// Call the `hisRead` op to read time-series history for a point.
    ///
    /// `id` is the point ref (e.g. `"@sensor-1"`). `range` is a Haystack range
    /// string: `"today"`, `"yesterday"`, a single date like `"2024-01-01"`, a
    /// date span `"2024-01-01,2024-01-31"`, or a datetime with timezone like
    /// `"2024-01-01T00:00:00-05:00 New_York"`. Returns a grid of `ts`/`val` rows.
    pub async fn his_read(&self, id: &str, range: &str) -> Result<HGrid, ClientError> {
        let mut row = HDict::new();
        row.set("id", Kind::Ref(HRef::from_val(id)));
        row.set("range", Kind::Str(range.to_string()));
        let grid = HGrid::from_parts(
            HDict::new(),
            vec![HCol::new("id"), HCol::new("range")],
            vec![row],
        );
        self.call("hisRead", &grid).await
    }

    /// Call the `hisWrite` op to write time-series samples for a point.
    ///
    /// `id` is the point ref. `items` must be dicts each containing a `ts`
    /// (DateTime) and `val` tag. Returns an empty grid on success or an
    /// error grid if the write is rejected.
    pub async fn his_write(&self, id: &str, items: Vec<HDict>) -> Result<HGrid, ClientError> {
        let mut meta = HDict::new();
        meta.set("id", Kind::Ref(HRef::from_val(id)));
        let grid = HGrid::from_parts(meta, vec![HCol::new("ts"), HCol::new("val")], items);
        self.call("hisWrite", &grid).await
    }

    /// Call the `invokeAction` op to invoke a named action on an entity.
    ///
    /// `id` is the target entity ref. `action` is the action name. `args` is
    /// an [`HDict`] of additional parameters for the action. Returns the
    /// action's result grid.
    pub async fn invoke_action(
        &self,
        id: &str,
        action: &str,
        args: HDict,
    ) -> Result<HGrid, ClientError> {
        let mut row = args;
        row.set("id", Kind::Ref(HRef::from_val(id)));
        row.set("action", Kind::Str(action.to_string()));
        let grid = HGrid::from_parts(
            HDict::new(),
            vec![HCol::new("id"), HCol::new("action")],
            vec![row],
        );
        self.call("invokeAction", &grid).await
    }

    /// Call the `close` op to close the current server session.
    ///
    /// This is distinct from [`close`](Self::close) which shuts down the
    /// underlying transport connection.
    pub async fn close_session(&self) -> Result<HGrid, ClientError> {
        self.call("close", &HGrid::new()).await
    }

    // -----------------------------------------------------------------------
    // Library & spec management ops
    // -----------------------------------------------------------------------

    /// List all Xeto specs on the server, optionally filtered by library name.
    ///
    /// Pass `None` to list specs across all libraries, or `Some("lib_name")`
    /// to restrict to a single library. Returns a grid of spec dicts.
    pub async fn specs(&self, lib: Option<&str>) -> Result<HGrid, ClientError> {
        let mut row = HDict::new();
        if let Some(lib_name) = lib {
            row.set("lib", Kind::Str(lib_name.to_string()));
        }
        let grid = HGrid::from_parts(HDict::new(), vec![HCol::new("lib")], vec![row]);
        self.call("specs", &grid).await
    }

    /// Get a single Xeto spec by its fully-qualified name.
    ///
    /// `qname` is the qualified spec name (e.g. `"ph::Site"`). Returns a
    /// single-row grid with the spec definition.
    pub async fn spec(&self, qname: &str) -> Result<HGrid, ClientError> {
        let mut row = HDict::new();
        row.set("qname", Kind::Str(qname.to_string()));
        let grid = HGrid::from_parts(HDict::new(), vec![HCol::new("qname")], vec![row]);
        self.call("spec", &grid).await
    }

    /// Load a Xeto library from source text into the server's ontology.
    ///
    /// `name` is the library name and `source` is the raw Xeto source code.
    /// Returns an error grid if the source fails to parse or validate.
    pub async fn load_lib(&self, name: &str, source: &str) -> Result<HGrid, ClientError> {
        let mut row = HDict::new();
        row.set("name", Kind::Str(name.to_string()));
        row.set("source", Kind::Str(source.to_string()));
        let grid = HGrid::from_parts(
            HDict::new(),
            vec![HCol::new("name"), HCol::new("source")],
            vec![row],
        );
        self.call("loadLib", &grid).await
    }

    /// Unload a previously loaded Xeto library by name.
    ///
    /// Removes the library and its specs from the server's active ontology.
    pub async fn unload_lib(&self, name: &str) -> Result<HGrid, ClientError> {
        let mut row = HDict::new();
        row.set("name", Kind::Str(name.to_string()));
        let grid = HGrid::from_parts(HDict::new(), vec![HCol::new("name")], vec![row]);
        self.call("unloadLib", &grid).await
    }

    /// Export a library as Xeto source text.
    ///
    /// Returns a grid whose first row contains the serialized Xeto source
    /// for the named library.
    pub async fn export_lib(&self, name: &str) -> Result<HGrid, ClientError> {
        let mut row = HDict::new();
        row.set("name", Kind::Str(name.to_string()));
        let grid = HGrid::from_parts(HDict::new(), vec![HCol::new("name")], vec![row]);
        self.call("exportLib", &grid).await
    }

    /// Validate entity dicts against the server's Xeto ontology.
    ///
    /// `entities` is a list of [`HDict`] records to check. Returns a grid
    /// with validation results — each row reports errors for the corresponding
    /// input entity. An empty result grid means all entities are valid.
    pub async fn validate(&self, entities: Vec<HDict>) -> Result<HGrid, ClientError> {
        // Build column set from all entities
        let mut col_names: Vec<String> = Vec::new();
        let mut seen = HashSet::new();
        for entity in &entities {
            for name in entity.tag_names() {
                if seen.insert(name.to_string()) {
                    col_names.push(name.to_string());
                }
            }
        }
        col_names.sort();
        let cols: Vec<HCol> = col_names.iter().map(|n| HCol::new(n.as_str())).collect();
        let grid = HGrid::from_parts(HDict::new(), cols, entities);
        self.call("validate", &grid).await
    }

    /// Close the underlying transport connection (HTTP or WebSocket).
    ///
    /// Call [`close_session`](Self::close_session) first if you want to
    /// explicitly end the server-side session before disconnecting.
    pub async fn close(&self) -> Result<(), ClientError> {
        self.transport.close().await
    }
}
