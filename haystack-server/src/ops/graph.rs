//! Graph visualization endpoints for React Flow UI integration.
//!
//! These endpoints expose the entity graph structure (nodes and edges) in
//! formats optimized for graph visualization libraries like React Flow.
//!
//! # Endpoints
//!
//! | Route                     | Method | Description                              |
//! |---------------------------|--------|------------------------------------------|
//! | `/api/graph/flow`         | POST   | Full graph as nodes + edges for React Flow |
//! | `/api/graph/edges`        | POST   | All ref relationships as explicit edges  |
//! | `/api/graph/tree`         | POST   | Recursive subtree from a root entity     |
//! | `/api/graph/neighbors`    | POST   | N-hop neighborhood around an entity      |
//! | `/api/graph/path`         | POST   | Shortest path between two entities       |
//! | `/api/graph/stats`        | GET    | Graph metrics and statistics             |
//!
//! # React Flow Data Model
//!
//! React Flow expects two arrays: `nodes` and `edges`.
//!
//! - **Node**: `{ id, type, data, position: { x, y }, parentId? }`
//! - **Edge**: `{ id, source, target, label, type }`
//!
//! The `graph/flow` endpoint returns two grids: a nodes grid and an edges
//! grid (edges encoded in the response grid's metadata under `edgesGrid`).
//! When `Accept: application/json` is used, it returns the native React Flow
//! JSON structure instead.

use actix_web::{HttpRequest, HttpResponse, web};
use std::collections::{HashMap, HashSet};

use haystack_core::data::{HCol, HDict, HGrid};
use haystack_core::kinds::{HRef, Kind, Number};

use crate::content;
use crate::error::HaystackError;
use crate::state::AppState;

// ── POST /api/graph/flow ──

/// Returns the entity graph formatted for React Flow consumption.
///
/// # Request Grid Columns
///
/// | Column  | Kind   | Description                                   |
/// |---------|--------|-----------------------------------------------|
/// | `filter`| Str    | *(optional)* Filter expression to scope nodes |
/// | `root`  | Ref    | *(optional)* Root entity for scoped subgraph  |
/// | `depth` | Number | *(optional)* Max depth from root (default 10) |
///
/// # Response
///
/// Returns a nodes grid with columns: `nodeId`, `nodeType`, `dis`,
/// `posX`, `posY`, `parentId`, plus all entity tags. The grid metadata
/// contains an `edges` tag with a nested grid of edges.
pub async fn handle_flow(
    req: HttpRequest,
    body: String,
    state: web::Data<AppState>,
) -> Result<HttpResponse, HaystackError> {
    let content_type = req
        .headers()
        .get("Content-Type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let accept = req
        .headers()
        .get("Accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // Parse optional request params
    let (filter, root, depth) = if body.trim().is_empty() {
        (None, None, 10usize)
    } else {
        let rg = content::decode_request_grid(&body, content_type)
            .map_err(|e| HaystackError::bad_request(format!("failed to decode request: {e}")))?;
        let row = rg.row(0);
        let filter = row.and_then(|r| match r.get("filter") {
            Some(Kind::Str(s)) if !s.is_empty() => Some(s.clone()),
            _ => None,
        });
        let root = row.and_then(|r| match r.get("root") {
            Some(Kind::Ref(r)) => Some(r.val.clone()),
            _ => None,
        });
        let depth = row
            .and_then(|r| match r.get("depth") {
                Some(Kind::Number(n)) => Some(n.val as usize),
                _ => None,
            })
            .unwrap_or(10);
        (filter, root, depth)
    };

    // Collect entities
    let entities: Vec<HDict> = match (&root, &filter) {
        (Some(root_id), _) => state
            .graph
            .subtree(root_id, depth)
            .into_iter()
            .map(|(e, _)| e)
            .collect(),
        (None, Some(f)) => {
            let f = if f == "*" {
                return Ok(build_flow_all(&state, accept)?);
            } else {
                f
            };
            state
                .graph
                .read_all(f, 0)
                .map_err(|e| HaystackError::bad_request(format!("filter error: {e}")))?
        }
        (None, None) => state.graph.all_entities(),
    };

    // Collect all edges between the selected entities
    let entity_ids: HashSet<String> = entities
        .iter()
        .filter_map(|e| e.id().map(|r| r.val.clone()))
        .collect();

    let all_edges = state.graph.all_edges();
    let edges: Vec<(String, String, String)> = all_edges
        .into_iter()
        .filter(|(src, _, tgt)| entity_ids.contains(src) && entity_ids.contains(tgt))
        .collect();

    build_flow_response(&entities, &edges, accept)
}

/// Build flow response for all entities (wildcard).
fn build_flow_all(state: &AppState, accept: &str) -> Result<HttpResponse, HaystackError> {
    let entities = state.graph.all_entities();
    let edges = state.graph.all_edges();
    build_flow_response(&entities, &edges, accept)
}

/// Build the flow response (nodes grid + edges grid).
fn build_flow_response(
    entities: &[HDict],
    edges: &[(String, String, String)],
    accept: &str,
) -> Result<HttpResponse, HaystackError> {
    if entities.is_empty() {
        let (encoded, ct) = content::encode_response_grid(&HGrid::new(), accept)
            .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;
        return Ok(HttpResponse::Ok().content_type(ct).body(encoded));
    }

    // Classify entity types for layered layout
    let mut type_depths: HashMap<String, usize> = HashMap::new();
    for entity in entities {
        let id = entity.id().map(|r| r.val.clone()).unwrap_or_default();
        let depth = entity_depth(entity);
        type_depths.insert(id, depth);
    }

    // Group by depth for x-positioning
    let mut depth_counts: HashMap<usize, usize> = HashMap::new();

    // Build nodes grid
    let mut node_rows: Vec<HDict> = Vec::with_capacity(entities.len());
    let mut all_tags: HashSet<String> = HashSet::new();

    for entity in entities {
        let id = entity.id().map(|r| r.val.clone()).unwrap_or_default();
        let depth = type_depths.get(&id).copied().unwrap_or(0);
        let x_idx = depth_counts.entry(depth).or_insert(0);
        let pos_x = (*x_idx as f64) * 280.0;
        let pos_y = (depth as f64) * 200.0;
        *x_idx += 1;

        let mut row = entity.clone();
        row.set("nodeId", Kind::Str(id.clone()));
        row.set("nodeType", Kind::Str(entity_type_name(entity)));
        row.set("posX", Kind::Number(Number::unitless(pos_x)));
        row.set("posY", Kind::Number(Number::unitless(pos_y)));

        // Set parentId from hierarchy refs
        if let Some(parent) = find_parent_ref(entity) {
            row.set("parentId", Kind::Str(parent));
        }

        for name in row.tag_names() {
            all_tags.insert(name.to_string());
        }
        node_rows.push(row);
    }

    // Build edges grid
    let edge_rows: Vec<HDict> = edges
        .iter()
        .map(|(src, tag, tgt)| {
            let mut row = HDict::new();
            row.set("edgeId", Kind::Str(format!("{src}:{tag}:{tgt}")));
            row.set("source", Kind::Str(src.clone()));
            row.set("target", Kind::Str(tgt.clone()));
            row.set("label", Kind::Str(tag.clone()));
            row
        })
        .collect();

    let edge_cols = vec![
        HCol::new("edgeId"),
        HCol::new("source"),
        HCol::new("target"),
        HCol::new("label"),
    ];
    let edges_grid = HGrid::from_parts(HDict::new(), edge_cols, edge_rows);

    // Ensure standard columns are first, then sorted remaining
    let mut sorted_tags: Vec<String> = all_tags.into_iter().collect();
    sorted_tags.sort();
    let cols: Vec<HCol> = sorted_tags.iter().map(|n| HCol::new(n.as_str())).collect();

    // Encode edges grid as Zinc and put in metadata
    let mut meta = HDict::new();
    let edges_zinc = haystack_core::codecs::codec_for("text/zinc")
        .map(|c| c.encode_grid(&edges_grid).unwrap_or_default())
        .unwrap_or_default();
    meta.set("edges", Kind::Str(edges_zinc));

    let nodes_grid = HGrid::from_parts(meta, cols, node_rows);

    let (encoded, ct) = content::encode_response_grid(&nodes_grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;

    Ok(HttpResponse::Ok().content_type(ct).body(encoded))
}

// ── POST /api/graph/edges ──

/// Returns all ref relationships as explicit edge rows.
///
/// # Request Grid Columns
///
/// | Column    | Kind | Description                                    |
/// |-----------|------|------------------------------------------------|
/// | `filter`  | Str  | *(optional)* Filter to scope source entities   |
/// | `refType` | Str  | *(optional)* Only edges of this ref tag type   |
pub async fn handle_edges(
    req: HttpRequest,
    body: String,
    state: web::Data<AppState>,
) -> Result<HttpResponse, HaystackError> {
    let content_type = req
        .headers()
        .get("Content-Type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let accept = req
        .headers()
        .get("Accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let (filter, ref_type) = if body.trim().is_empty() {
        (None, None)
    } else {
        let rg = content::decode_request_grid(&body, content_type)
            .map_err(|e| HaystackError::bad_request(format!("failed to decode request: {e}")))?;
        let row = rg.row(0);
        let filter = row.and_then(|r| match r.get("filter") {
            Some(Kind::Str(s)) if !s.is_empty() => Some(s.clone()),
            _ => None,
        });
        let ref_type = row.and_then(|r| match r.get("refType") {
            Some(Kind::Str(s)) if !s.is_empty() => Some(s.clone()),
            _ => None,
        });
        (filter, ref_type)
    };

    // Get all edges from graph
    let all_edges = state.graph.all_edges();

    // Filter edges if needed
    let entity_ids: Option<HashSet<String>> = filter.map(|f| {
        if f == "*" {
            return state
                .graph
                .all_entities()
                .into_iter()
                .filter_map(|e| e.id().map(|r| r.val.clone()))
                .collect();
        }
        state
            .graph
            .read_all(&f, 0)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|e| e.id().map(|r| r.val.clone()))
            .collect()
    });

    let edges: Vec<(String, String, String)> = all_edges
        .into_iter()
        .filter(|(src, tag, _)| {
            if let Some(ref ids) = entity_ids {
                if !ids.contains(src) {
                    return false;
                }
            }
            if let Some(ref rt) = ref_type {
                if tag != rt {
                    return false;
                }
            }
            true
        })
        .collect();

    if edges.is_empty() {
        let (encoded, ct) = content::encode_response_grid(&HGrid::new(), accept)
            .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;
        return Ok(HttpResponse::Ok().content_type(ct).body(encoded));
    }

    let cols = vec![
        HCol::new("id"),
        HCol::new("source"),
        HCol::new("target"),
        HCol::new("refTag"),
    ];
    let rows: Vec<HDict> = edges
        .iter()
        .map(|(src, tag, tgt)| {
            let mut row = HDict::new();
            row.set("id", Kind::Str(format!("{src}:{tag}:{tgt}")));
            row.set("source", Kind::Ref(HRef::from_val(src)));
            row.set("target", Kind::Ref(HRef::from_val(tgt)));
            row.set("refTag", Kind::Str(tag.clone()));
            row
        })
        .collect();

    let grid = HGrid::from_parts(HDict::new(), cols, rows);
    let (encoded, ct) = content::encode_response_grid(&grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;

    Ok(HttpResponse::Ok().content_type(ct).body(encoded))
}

// ── POST /api/graph/tree ──

/// Returns a hierarchical subtree from a root entity.
///
/// # Request Grid Columns
///
/// | Column     | Kind   | Description                                  |
/// |------------|--------|----------------------------------------------|
/// | `root`     | Ref    | Root entity to start tree from               |
/// | `maxDepth` | Number | *(optional)* Maximum tree depth (default 10) |
///
/// # Response Grid Columns
///
/// All entity tags plus: `depth`, `parentId`, `navId`.
pub async fn handle_tree(
    req: HttpRequest,
    body: String,
    state: web::Data<AppState>,
) -> Result<HttpResponse, HaystackError> {
    let content_type = req
        .headers()
        .get("Content-Type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let accept = req
        .headers()
        .get("Accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let rg = content::decode_request_grid(&body, content_type)
        .map_err(|e| HaystackError::bad_request(format!("failed to decode request: {e}")))?;

    let row = rg
        .row(0)
        .ok_or_else(|| HaystackError::bad_request("request grid has no rows"))?;

    let root = match row.get("root") {
        Some(Kind::Ref(r)) => r.val.clone(),
        Some(Kind::Str(s)) => s.clone(),
        _ => return Err(HaystackError::bad_request("'root' Ref is required")),
    };

    let max_depth = match row.get("maxDepth") {
        Some(Kind::Number(n)) => n.val as usize,
        _ => 10,
    };

    // Verify root exists
    if !state.graph.contains(&root) {
        return Err(HaystackError::not_found(format!(
            "root entity not found: {root}"
        )));
    }

    let subtree = state.graph.subtree(&root, max_depth);

    if subtree.is_empty() {
        let (encoded, ct) = content::encode_response_grid(&HGrid::new(), accept)
            .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;
        return Ok(HttpResponse::Ok().content_type(ct).body(encoded));
    }

    // Build parent map from ref tags
    let parent_map = build_parent_map(&subtree, &state);

    let mut all_tags: HashSet<String> = HashSet::new();
    let mut rows: Vec<HDict> = Vec::with_capacity(subtree.len());

    for (entity, depth) in &subtree {
        let mut row = entity.clone();
        row.set("depth", Kind::Number(Number::unitless(*depth as f64)));

        let id = entity.id().map(|r| r.val.clone()).unwrap_or_default();
        if let Some(parent) = parent_map.get(&id) {
            row.set("parentId", Kind::Ref(HRef::from_val(parent)));
        }
        row.set("navId", Kind::Str(id));

        for name in row.tag_names() {
            all_tags.insert(name.to_string());
        }
        rows.push(row);
    }

    let mut sorted_tags: Vec<String> = all_tags.into_iter().collect();
    sorted_tags.sort();
    let cols: Vec<HCol> = sorted_tags.iter().map(|n| HCol::new(n.as_str())).collect();
    let grid = HGrid::from_parts(HDict::new(), cols, rows);

    let (encoded, ct) = content::encode_response_grid(&grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;

    Ok(HttpResponse::Ok().content_type(ct).body(encoded))
}

// ── POST /api/graph/neighbors ──

/// Returns N-hop neighborhood around an entity.
///
/// # Request Grid Columns
///
/// | Column     | Kind   | Description                                    |
/// |------------|--------|------------------------------------------------|
/// | `id`       | Ref    | Center entity                                  |
/// | `hops`     | Number | *(optional)* Traversal depth (default 1)       |
/// | `refTypes` | Str    | *(optional)* Comma-separated ref types to follow |
///
/// # Response
///
/// Nodes grid with all entity tags. Edges encoded in grid metadata.
pub async fn handle_neighbors(
    req: HttpRequest,
    body: String,
    state: web::Data<AppState>,
) -> Result<HttpResponse, HaystackError> {
    let content_type = req
        .headers()
        .get("Content-Type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let accept = req
        .headers()
        .get("Accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let rg = content::decode_request_grid(&body, content_type)
        .map_err(|e| HaystackError::bad_request(format!("failed to decode request: {e}")))?;

    let row = rg
        .row(0)
        .ok_or_else(|| HaystackError::bad_request("request grid has no rows"))?;

    let id = match row.get("id") {
        Some(Kind::Ref(r)) => r.val.clone(),
        Some(Kind::Str(s)) => s.clone(),
        _ => return Err(HaystackError::bad_request("'id' Ref is required")),
    };

    let hops = match row.get("hops") {
        Some(Kind::Number(n)) => n.val as usize,
        _ => 1,
    };

    let ref_types_str: Option<String> = match row.get("refTypes") {
        Some(Kind::Str(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    };

    if !state.graph.contains(&id) {
        return Err(HaystackError::not_found(format!("entity not found: {id}")));
    }

    let ref_types_vec: Option<Vec<String>> =
        ref_types_str.map(|s| s.split(',').map(|t| t.trim().to_string()).collect());
    let ref_types_refs: Option<Vec<&str>> = ref_types_vec
        .as_ref()
        .map(|v| v.iter().map(|s| s.as_str()).collect());

    let (entities, edges) = state.graph.neighbors(&id, hops, ref_types_refs.as_deref());

    build_flow_response(&entities, &edges, accept)
}

// ── POST /api/graph/path ──

/// Finds the shortest path between two entities.
///
/// # Request Grid Columns
///
/// | Column | Kind | Description         |
/// |--------|------|---------------------|
/// | `from` | Ref  | Source entity       |
/// | `to`   | Ref  | Destination entity  |
///
/// # Response Grid Columns
///
/// All entity tags plus `pathIndex` (Number, 0-based position in path).
pub async fn handle_path(
    req: HttpRequest,
    body: String,
    state: web::Data<AppState>,
) -> Result<HttpResponse, HaystackError> {
    let content_type = req
        .headers()
        .get("Content-Type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let accept = req
        .headers()
        .get("Accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let rg = content::decode_request_grid(&body, content_type)
        .map_err(|e| HaystackError::bad_request(format!("failed to decode request: {e}")))?;

    let row = rg
        .row(0)
        .ok_or_else(|| HaystackError::bad_request("request grid has no rows"))?;

    let from = match row.get("from") {
        Some(Kind::Ref(r)) => r.val.clone(),
        Some(Kind::Str(s)) => s.clone(),
        _ => return Err(HaystackError::bad_request("'from' Ref is required")),
    };

    let to = match row.get("to") {
        Some(Kind::Ref(r)) => r.val.clone(),
        Some(Kind::Str(s)) => s.clone(),
        _ => return Err(HaystackError::bad_request("'to' Ref is required")),
    };

    let path = state.graph.shortest_path(&from, &to);

    if path.is_empty() {
        let (encoded, ct) = content::encode_response_grid(&HGrid::new(), accept)
            .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;
        return Ok(HttpResponse::Ok().content_type(ct).body(encoded));
    }

    let mut all_tags: HashSet<String> = HashSet::new();
    let mut rows: Vec<HDict> = Vec::with_capacity(path.len());

    for (idx, ref_val) in path.iter().enumerate() {
        let mut row = state.graph.get(ref_val).unwrap_or_else(|| {
            let mut stub = HDict::new();
            stub.set("id", Kind::Ref(HRef::from_val(ref_val)));
            stub
        });
        row.set("pathIndex", Kind::Number(Number::unitless(idx as f64)));
        for name in row.tag_names() {
            all_tags.insert(name.to_string());
        }
        rows.push(row);
    }

    let mut sorted_tags: Vec<String> = all_tags.into_iter().collect();
    sorted_tags.sort();
    let cols: Vec<HCol> = sorted_tags.iter().map(|n| HCol::new(n.as_str())).collect();
    let grid = HGrid::from_parts(HDict::new(), cols, rows);

    let (encoded, ct) = content::encode_response_grid(&grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;

    Ok(HttpResponse::Ok().content_type(ct).body(encoded))
}

// ── GET /api/graph/stats ──

/// Returns graph statistics and metrics.
///
/// # Response Grid Columns
///
/// | Column  | Kind   | Description              |
/// |---------|--------|--------------------------|
/// | `metric`| Str    | Metric name              |
/// | `value` | Number | Metric value             |
/// | `detail`| Str    | *(optional)* Breakdown   |
pub async fn handle_stats(state: web::Data<AppState>) -> Result<HttpResponse, HaystackError> {
    let entities = state.graph.all_entities();
    let edges = state.graph.all_edges();

    // Count entity types
    let mut type_counts: HashMap<String, usize> = HashMap::new();
    for entity in &entities {
        let etype = entity_type_name(entity);
        *type_counts.entry(etype).or_insert(0) += 1;
    }

    // Count ref types
    let mut ref_counts: HashMap<String, usize> = HashMap::new();
    for (_, tag, _) in &edges {
        *ref_counts.entry(tag.clone()).or_insert(0) += 1;
    }

    // Connected components (union-find)
    let component_count = count_components(&entities, &edges);

    let cols = vec![HCol::new("metric"), HCol::new("value"), HCol::new("detail")];

    let mut rows: Vec<HDict> = Vec::new();

    // Total entities
    let mut row = HDict::new();
    row.set("metric", Kind::Str("totalEntities".into()));
    row.set(
        "value",
        Kind::Number(Number::unitless(entities.len() as f64)),
    );
    rows.push(row);

    // Total edges
    let mut row = HDict::new();
    row.set("metric", Kind::Str("totalEdges".into()));
    row.set("value", Kind::Number(Number::unitless(edges.len() as f64)));
    rows.push(row);

    // Connected components
    let mut row = HDict::new();
    row.set("metric", Kind::Str("connectedComponents".into()));
    row.set(
        "value",
        Kind::Number(Number::unitless(component_count as f64)),
    );
    rows.push(row);

    // Entity type breakdown
    let mut type_entries: Vec<_> = type_counts.into_iter().collect();
    type_entries.sort_by(|a, b| b.1.cmp(&a.1));
    for (etype, count) in type_entries {
        let mut row = HDict::new();
        row.set("metric", Kind::Str("entityType".into()));
        row.set("value", Kind::Number(Number::unitless(count as f64)));
        row.set("detail", Kind::Str(etype));
        rows.push(row);
    }

    // Ref type breakdown
    let mut ref_entries: Vec<_> = ref_counts.into_iter().collect();
    ref_entries.sort_by(|a, b| b.1.cmp(&a.1));
    for (rtype, count) in ref_entries {
        let mut row = HDict::new();
        row.set("metric", Kind::Str("refType".into()));
        row.set("value", Kind::Number(Number::unitless(count as f64)));
        row.set("detail", Kind::Str(rtype));
        rows.push(row);
    }

    let grid = HGrid::from_parts(HDict::new(), cols, rows);

    // Stats always returns JSON-compatible Zinc
    let accept = "text/zinc";
    let (encoded, ct) = content::encode_response_grid(&grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;

    Ok(HttpResponse::Ok().content_type(ct).body(encoded))
}

// ── Helpers ──

/// Determine the Haystack entity type for React Flow node typing.
fn entity_type_name(entity: &HDict) -> String {
    // Check common Haystack marker tags in priority order
    for tag in &[
        "site", "space", "floor", "wing", "equip", "point", "device", "conn", "weather",
    ] {
        if entity.has(tag) {
            return (*tag).to_string();
        }
    }
    "entity".to_string()
}

/// Determine layout depth based on entity type (for layered layout).
fn entity_depth(entity: &HDict) -> usize {
    if entity.has("site") {
        0
    } else if entity.has("space") || entity.has("floor") || entity.has("wing") {
        1
    } else if entity.has("equip") {
        2
    } else if entity.has("point") {
        3
    } else {
        1 // Default middle layer
    }
}

/// Find the primary parent ref for an entity (for React Flow parentId).
fn find_parent_ref(entity: &HDict) -> Option<String> {
    // Check hierarchy refs in priority order
    for tag in &["equipRef", "spaceRef", "siteRef"] {
        if let Some(Kind::Ref(r)) = entity.get(tag) {
            return Some(r.val.clone());
        }
    }
    None
}

/// Build a parent map: entity_id -> parent_id from ref tags in the subtree.
fn build_parent_map(subtree: &[(HDict, usize)], state: &AppState) -> HashMap<String, String> {
    let mut parent_map = HashMap::new();
    let subtree_ids: HashSet<String> = subtree
        .iter()
        .filter_map(|(e, _)| e.id().map(|r| r.val.clone()))
        .collect();

    for (entity, _) in subtree {
        let id = match entity.id() {
            Some(r) => r.val.clone(),
            None => continue,
        };
        if let Some(parent) = find_parent_ref(entity) {
            if subtree_ids.contains(&parent) {
                parent_map.insert(id, parent);
            }
        }
    }
    let _ = state; // state available if needed for extended lookup
    parent_map
}

/// Count connected components using union-find.
fn count_components(entities: &[HDict], edges: &[(String, String, String)]) -> usize {
    if entities.is_empty() {
        return 0;
    }

    let mut id_to_idx: HashMap<String, usize> = HashMap::new();
    for (i, entity) in entities.iter().enumerate() {
        if let Some(r) = entity.id() {
            id_to_idx.insert(r.val.clone(), i);
        }
    }

    let n = entities.len();
    let mut parent: Vec<usize> = (0..n).collect();
    let mut rank: Vec<usize> = vec![0; n];

    fn find(parent: &mut [usize], x: usize) -> usize {
        if parent[x] != x {
            parent[x] = find(parent, parent[x]);
        }
        parent[x]
    }

    fn union(parent: &mut [usize], rank: &mut [usize], x: usize, y: usize) {
        let rx = find(parent, x);
        let ry = find(parent, y);
        if rx != ry {
            if rank[rx] < rank[ry] {
                parent[rx] = ry;
            } else if rank[rx] > rank[ry] {
                parent[ry] = rx;
            } else {
                parent[ry] = rx;
                rank[rx] += 1;
            }
        }
    }

    for (src, _, tgt) in edges {
        if let (Some(&si), Some(&ti)) = (id_to_idx.get(src), id_to_idx.get(tgt)) {
            union(&mut parent, &mut rank, si, ti);
        }
    }

    let mut roots: HashSet<usize> = HashSet::new();
    for i in 0..n {
        roots.insert(find(&mut parent, i));
    }
    roots.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_site(id: &str) -> HDict {
        let mut d = HDict::new();
        d.set("id", Kind::Ref(HRef::from_val(id)));
        d.set("site", Kind::Marker);
        d.set("dis", Kind::Str(format!("Site {id}")));
        d
    }

    fn make_equip(id: &str, site_ref: &str) -> HDict {
        let mut d = HDict::new();
        d.set("id", Kind::Ref(HRef::from_val(id)));
        d.set("equip", Kind::Marker);
        d.set("siteRef", Kind::Ref(HRef::from_val(site_ref)));
        d.set("dis", Kind::Str(format!("Equip {id}")));
        d
    }

    fn make_point(id: &str, equip_ref: &str, site_ref: &str) -> HDict {
        let mut d = HDict::new();
        d.set("id", Kind::Ref(HRef::from_val(id)));
        d.set("point", Kind::Marker);
        d.set("equipRef", Kind::Ref(HRef::from_val(equip_ref)));
        d.set("siteRef", Kind::Ref(HRef::from_val(site_ref)));
        d.set("dis", Kind::Str(format!("Point {id}")));
        d
    }

    #[test]
    fn entity_type_detection() {
        assert_eq!(entity_type_name(&make_site("s1")), "site");
        assert_eq!(entity_type_name(&make_equip("e1", "s1")), "equip");
        assert_eq!(entity_type_name(&make_point("p1", "e1", "s1")), "point");

        let empty = HDict::new();
        assert_eq!(entity_type_name(&empty), "entity");
    }

    #[test]
    fn entity_depth_classification() {
        assert_eq!(entity_depth(&make_site("s1")), 0);
        assert_eq!(entity_depth(&make_equip("e1", "s1")), 2);
        assert_eq!(entity_depth(&make_point("p1", "e1", "s1")), 3);
    }

    #[test]
    fn parent_ref_detection() {
        let equip = make_equip("e1", "s1");
        assert_eq!(find_parent_ref(&equip), Some("s1".to_string()));

        let point = make_point("p1", "e1", "s1");
        // equipRef has priority over siteRef
        assert_eq!(find_parent_ref(&point), Some("e1".to_string()));

        let site = make_site("s1");
        assert_eq!(find_parent_ref(&site), None);
    }

    #[test]
    fn connected_components_single() {
        let entities = vec![make_site("s1"), make_equip("e1", "s1")];
        let edges = vec![("e1".into(), "siteRef".into(), "s1".into())];
        assert_eq!(count_components(&entities, &edges), 1);
    }

    #[test]
    fn connected_components_disjoint() {
        let entities = vec![make_site("s1"), make_site("s2")];
        let edges: Vec<(String, String, String)> = vec![];
        assert_eq!(count_components(&entities, &edges), 2);
    }

    #[test]
    fn connected_components_empty() {
        let entities: Vec<HDict> = vec![];
        let edges: Vec<(String, String, String)> = vec![];
        assert_eq!(count_components(&entities, &edges), 0);
    }
}
