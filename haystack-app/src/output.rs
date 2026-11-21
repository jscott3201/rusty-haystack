use crate::{BudgetKind, H4Codec, OutputProfile, ReadError, ReadOutput, budget::Budget};
use haystack_core::{
    codecs::codec_for,
    data::{HCol, HDict, HGrid},
    kinds::Kind,
};
use std::collections::BTreeSet;

pub(crate) fn grid(
    rows: Vec<HDict>,
    complete: bool,
    cursor: Option<&str>,
    budget: &mut Budget,
) -> Result<HGrid, ReadError> {
    let mut names = BTreeSet::new();
    for row in &rows {
        for tag in row.tag_names() {
            budget.charge(BudgetKind::Work, 1)?;
            if !names.contains(tag) {
                budget.charge(BudgetKind::Retained, tag.len().saturating_add(128))?;
                names.insert(tag);
            }
        }
    }
    let mut cols = Vec::new();
    for name in names {
        budget.charge(BudgetKind::Retained, 256)?;
        cols.push(HCol::new(budget.copy_string(name)?));
    }
    let mut meta = HDict::new();
    meta.set("complete", Kind::Bool(complete));
    if let Some(cursor) = cursor {
        meta.set("cursor", Kind::Str(budget.copy_string(cursor)?));
    }
    budget.charge(BudgetKind::Retained, 1024)?;
    Ok(HGrid::from_parts(meta, cols, rows))
}

/// Conservative upper bounds are charged before invoking allocating legacy
/// codecs. Include the column-by-row cross product and every value, including
/// fields a particular codec discards. All additions/multiplications saturate
/// toward rejection; no oversized scalar is formatted and then size-checked.
fn add(total: &mut usize, n: usize, budget: &mut Budget) -> Result<(), ReadError> {
    budget.charge(BudgetKind::Work, 1)?;
    *total = total.saturating_add(n);
    if *total > budget.limits.max_output_bytes {
        return Err(ReadError::Budget(BudgetKind::Output));
    }
    Ok(())
}
fn string_bound(text: &str) -> usize {
    text.len().saturating_mul(6).saturating_add(32)
}
fn dict_bound(
    dict: &HDict,
    total: &mut usize,
    budget: &mut Budget,
    depth: usize,
    strict: bool,
) -> Result<(), ReadError> {
    for (tag, value) in dict.iter() {
        add(total, string_bound(tag), budget)?;
        value_bound(value, total, budget, depth + 1, strict)?;
    }
    Ok(())
}
fn value_bound(
    value: &Kind,
    total: &mut usize,
    budget: &mut Budget,
    depth: usize,
    strict: bool,
) -> Result<(), ReadError> {
    budget.depth(depth)?;
    budget.charge(BudgetKind::Values, 1)?;
    let fixed = match value {
        Kind::Number(_) => 384,
        Kind::Coord(_) => 768,
        Kind::DateTime(_) | Kind::Nominal(_) => 128,
        _ => 32,
    };
    add(total, fixed, budget)?; // maximum scalar format/type/container overhead
    match value {
        Kind::None | Kind::Int(_) | Kind::Float(_) | Kind::Buf(_) | Kind::Nominal(_) if strict => {
            return Err(ReadError::Projection);
        }
        Kind::Str(s) => add(total, string_bound(s), budget)?,
        Kind::Number(n) => {
            if let Some(unit) = &n.unit {
                add(total, string_bound(unit), budget)?;
            }
        }
        Kind::Ref(r) => {
            add(total, string_bound(&r.val), budget)?;
            if let Some(dis) = &r.dis {
                add(total, string_bound(dis), budget)?;
            }
        }
        Kind::Uri(u) => add(total, string_bound(u.val()), budget)?,
        Kind::Symbol(s) => add(total, string_bound(s.val()), budget)?,
        Kind::DateTime(d) => add(total, string_bound(&d.tz_name), budget)?,
        Kind::XStr(x) => {
            add(total, string_bound(&x.type_name), budget)?;
            add(total, string_bound(&x.val), budget)?;
        }
        Kind::List(items) => {
            for value in items {
                value_bound(value, total, budget, depth + 1, strict)?;
            }
        }
        Kind::Dict(dict) => dict_bound(dict, total, budget, depth + 1, strict)?,
        Kind::Grid(grid) => grid_bound(grid, total, budget, depth + 1, strict)?,
        Kind::Buf(bytes) => add(total, bytes.len().saturating_mul(2), budget)?,
        Kind::Nominal(n) => {
            for s in [n.spec(), n.catalog(), n.revision(), n.text()] {
                add(total, string_bound(s), budget)?;
            }
        }
        _ => {}
    }
    Ok(())
}
fn grid_bound(
    grid: &HGrid,
    total: &mut usize,
    budget: &mut Budget,
    depth: usize,
    strict: bool,
) -> Result<(), ReadError> {
    budget.depth(depth)?;
    add(total, 512, budget)?;
    dict_bound(&grid.meta, total, budget, depth + 1, strict)?;
    for col in &grid.cols {
        add(total, string_bound(&col.name), budget)?;
        dict_bound(&col.meta, total, budget, depth + 1, strict)?;
    }
    for row in &grid.rows {
        dict_bound(row, total, budget, depth + 1, strict)?;
        for col in &grid.cols {
            // Some codecs key every cell by its column, including absent/null
            // cells; account for expansion from a wide sparse grid explicitly.
            add(total, string_bound(&col.name).saturating_add(32), budget)?;
        }
    }
    Ok(())
}

pub(crate) fn encode(
    grid: HGrid,
    profile: OutputProfile,
    budget: &mut Budget,
) -> Result<ReadOutput, ReadError> {
    if profile == OutputProfile::H4(H4Codec::Trio) {
        return Err(ReadError::InvalidQuery("codec cannot carry page metadata"));
    }
    let strict = matches!(profile, OutputProfile::H4(_));
    let mut bound = 0;
    grid_bound(&grid, &mut bound, budget, 1, strict)?;
    match profile {
        OutputProfile::Typed => Ok(ReadOutput::Typed(grid)),
        // Contextual output is only available through the coherent catalog
        // transaction below, never from an unbound post-read conversion.
        OutputProfile::Jeto => Err(ReadError::Unavailable),
        OutputProfile::H4(codec) => {
            // Legacy encoders build scalar strings, JSON trees and the final
            // buffer. This conservative reservation includes those temporaries.
            budget.charge(BudgetKind::Retained, bound.saturating_mul(8))?;
            let body = codec_for(codec.mime())
                .ok_or(ReadError::Unavailable)?
                .encode_grid(&grid)
                .map_err(|_| ReadError::Projection)?;
            budget.check()?;
            if body.len() > bound || body.len() > budget.limits.max_output_bytes {
                return Err(ReadError::Budget(BudgetKind::Output));
            }
            Ok(ReadOutput::H4 {
                body: body.into_bytes(),
                codec,
            })
        }
    }
}

pub(crate) fn jeto(
    grid: HGrid,
    observation: &haystack_core::xeto::catalog::ActivatedCatalog,
    budget: &mut Budget,
) -> Result<ReadOutput, ReadError> {
    use haystack_core::codecs::jeto;
    #[cfg(test)]
    if let Some(hook) = &budget.typed_encode_hook {
        hook();
    }
    budget.check()?;
    // The admitted readById codec context already contains every selected
    // concrete data declaration. Borrow only its codec context: this does not
    // execute that function or infer permission to invoke it.
    let context = &observation
        .callable("sys.api::readById")
        .ok_or(ReadError::Unavailable)?
        .context;
    let provenance = crate::ReadProvenance {
        repository: budget.copy_string(context.catalog())?,
        revision: budget.copy_string(context.revision())?,
        selection: budget.copy_string(observation.selection_identity())?,
    };
    let body = jeto::encode_metered(
        &Kind::Grid(Box::new(grid)),
        context,
        Some("sys::Grid"),
        jeto::Boxing::All,
        &mut crate::budget::JetoMeter(budget),
    )
    .map_err(|error| match error {
        jeto::Error::Budget(error) => error,
        jeto::Error::Allocation => ReadError::Unavailable,
        _ => ReadError::Projection,
    })?
    .into_exact()
    .map_err(|_| ReadError::Projection)?;
    let bytes = body
        .len()
        .saturating_add(provenance.repository.len())
        .saturating_add(provenance.revision.len())
        .saturating_add(provenance.selection.len());
    if bytes > budget.limits.max_output_bytes {
        return Err(ReadError::Budget(BudgetKind::Output));
    }
    budget.check()?;
    Ok(ReadOutput::Jeto { body, provenance })
}

#[cfg(test)]
mod rich_tests {
    use crate::*;
    use haystack_core::graph::{EntityGraph, SharedGraph};
    use std::{sync::Arc, time::Duration};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn interrupted_jeto_encoding_retains_admission_until_actual_worker_exit() {
        for deadline in [false, true] {
            let service = ReadService::new(
                SharedGraph::new(EntityGraph::new()),
                Arc::new(AllowAll),
                ReadLimits {
                    max_concurrent: 1,
                    max_queued: 0,
                    ..Default::default()
                },
            )
            .unwrap();
            let cancel = CancellationToken::new();
            let mut admission = service
                .begin(ReadContext::new(
                    Principal::Anonymous,
                    std::time::Instant::now()
                        + Duration::from_millis(if deadline { 200 } else { 3000 }),
                    cancel.clone(),
                ))
                .await
                .unwrap();
            let (entered_tx, entered_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let release_rx = std::sync::Mutex::new(release_rx);
            admission.budget_mut().typed_encode_hook = Some(Arc::new(move || {
                entered_tx.send(()).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(3))
                    .unwrap();
            }));
            let worker = tokio::spawn(admission.read_joined(ReadRequest::new(
                ReadQuery::Filter("*".into()),
                OutputProfile::Jeto,
            )));
            entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
            if deadline {
                tokio::time::sleep(Duration::from_millis(250)).await;
            } else {
                cancel.cancel();
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert!(
                !worker.is_finished(),
                "joined read must retain its real worker"
            );
            assert_eq!(service.load().admitted, 1);
            release_tx.send(()).unwrap();
            assert_eq!(
                worker.await.unwrap().unwrap_err(),
                if deadline {
                    ReadError::Deadline
                } else {
                    ReadError::Cancelled
                }
            );
            assert_eq!(service.load().admitted, 0);
        }
    }
}
