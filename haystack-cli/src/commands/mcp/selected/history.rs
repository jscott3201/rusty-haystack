//! Explicit bounded startup data; this does not expose a write tool.
use super::*;
use haystack_core::codecs::{history::valid_value, typed};
use std::collections::BTreeMap;

pub(in crate::commands::mcp) fn select_history(
    builder: ApplicationBuilder,
    config: &Path,
    selected: Option<&Path>,
) -> Result<ApplicationBuilder, String> {
    let Some(file) = selected else {
        return Ok(builder);
    };
    let file = if file.is_absolute() {
        file.to_path_buf()
    } else {
        config.parent().unwrap_or(Path::new(".")).join(file)
    };
    let content = bounded(&file, 4 * 1024 * 1024)?;
    let value = typed::decode_with_limits(
        content.as_bytes(),
        typed::PayloadLimits {
            max_bytes: 4 * 1024 * 1024,
            max_depth: 8,
            max_nodes: 100_000,
        },
    )
    .map_err(|_| "invalid MCP history fixture")?;
    let Kind::Grid(grid) = value else {
        return Err("MCP history fixture must contain a Grid".into());
    };
    if !grid.meta.is_empty()
        || grid.rows.len() > 10_000
        || grid
            .cols
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>()
            != ["id", "ts", "val"]
        || grid.cols.iter().any(|c| !c.meta.is_empty())
    {
        return Err("invalid MCP history fixture shape".into());
    }
    let graph = builder.handle().read_service().graph();
    let mut series: BTreeMap<String, Vec<HisItem>> = BTreeMap::new();
    for row in &grid.rows {
        let (Some(Kind::Ref(id)), Some(Kind::DateTime(ts)), Some(val)) =
            (row.get("id"), row.get("ts"), row.get("val"))
        else {
            return Err("invalid MCP history sample".into());
        };
        if row.len() != 3 || id.dis.is_some() {
            return Err("invalid MCP history sample".into());
        }
        let point = graph
            .get(&id.val)
            .ok_or("history fixture point is unavailable")?;
        let schema =
            HistoryService::point_schema(&point).map_err(|_| "invalid MCP history point schema")?;
        if !valid_value(val, &schema) {
            return Err("unsupported MCP history sample".into());
        }
        let boundary = ts.to_string();
        HistoryService::finite_request(id.val.clone(), &boundary, &boundary)
            .map_err(|_| "invalid MCP history timestamp")?;
        let values = series.entry(id.val.clone()).or_default();
        if values.last().is_some_and(|previous| previous.ts >= ts.dt) {
            return Err("MCP history samples must be strictly ordered per point".into());
        }
        values.push(HisItem {
            ts: ts.dt,
            val: val.clone(),
        });
    }
    let store = Arc::new(
        HisStore::with_limits(HistoryStoreLimits {
            max_items_per_point: 10_000,
            max_points: 10_000,
            ..HistoryStoreLimits::default()
        })
        .map_err(|_| "invalid MCP history store limits")?,
    );
    for (id, values) in series {
        store
            .write(&id, values)
            .map_err(|_| "invalid MCP history fixture")?;
    }
    let history = HistoryService::new(
        builder.handle().read_service(),
        store,
        HistoryLimits::default(),
    )
    .map_err(|_| "invalid MCP history selection")?;
    builder
        .owned_history(history)
        .map_err(|_| "invalid MCP history ownership".into())
}
