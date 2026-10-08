use crate::{
    BudgetKind, H4Codec, OutputProfile, ReadError, ReadOperation, ReadQuery, ReadRequest,
    budget::Budget,
};
use haystack_core::{codecs::codec_for, data::HGrid, kinds::Kind};

pub(crate) fn decode(
    operation: ReadOperation,
    input: &[u8],
    codec: H4Codec,
    output: H4Codec,
    budget: &mut Budget,
) -> Result<ReadRequest, ReadError> {
    if output == H4Codec::Trio {
        return Err(ReadError::InvalidQuery("codec cannot carry page metadata"));
    }
    budget.check()?;
    if input.len() > budget.limits.max_input_bytes {
        return Err(ReadError::Budget(BudgetKind::Input));
    }
    // Decode only after admission. Reserve a conservative expansion bound before
    // legacy codec allocation. Zinc can duplicate column names into every row:
    // include maximum physical line length times line count before decoding.
    // This overcounts nested/multiline inputs rather than undercounting them.
    let text =
        std::str::from_utf8(input).map_err(|_| ReadError::InvalidQuery("request is not UTF-8"))?;
    let lines = text.lines().count().saturating_add(1);
    let longest = text.lines().map(str::len).max().unwrap_or(0);
    let expansion = input.len().saturating_add(if codec == H4Codec::Zinc {
        longest.saturating_mul(lines)
    } else {
        0
    });
    budget.charge(BudgetKind::Work, input.len().saturating_add(1))?;
    budget.charge(BudgetKind::Retained, expansion.saturating_mul(512))?;
    let grid = if text.trim().is_empty() {
        HGrid::new()
    } else {
        codec_for(codec.mime())
            .ok_or(ReadError::Unavailable)?
            .decode_grid(text)
            .map_err(|_| ReadError::InvalidQuery("invalid request grid"))?
    };
    budget.check()?;
    let first = grid.rows.first();
    let query = match operation {
        ReadOperation::Changes | ReadOperation::History => {
            return Err(ReadError::InvalidQuery(
                "changes requires entity-v1 payload",
            ));
        }
        ReadOperation::Read => {
            if grid.rows.iter().any(|r| r.has("id")) {
                if grid.rows.len() > budget.limits.max_ids {
                    return Err(ReadError::Budget(BudgetKind::Ids));
                }
                let mut ids = Vec::new();
                for row in &grid.rows {
                    budget.charge(BudgetKind::Work, 1)?;
                    let Some(Kind::Ref(id)) = row.get("id") else {
                        return Err(ReadError::InvalidQuery("id rows require Ref values"));
                    };
                    if row.has("filter") {
                        return Err(ReadError::InvalidQuery("mixed id and filter request"));
                    }
                    budget.charge(BudgetKind::Retained, 64)?;
                    ids.push(budget.copy_string(&id.val)?);
                }
                ReadQuery::Ids(ids)
            } else {
                if grid.rows.len() != 1 {
                    return Err(ReadError::InvalidQuery("filter request requires one row"));
                }
                let Some(Kind::Str(filter)) = first.and_then(|r| r.get("filter")) else {
                    return Err(ReadError::InvalidQuery("filter is required"));
                };
                ReadQuery::Filter(budget.copy_string(filter)?)
            }
        }
        ReadOperation::Nav => {
            let parent = match first.and_then(|r| r.get("navId")) {
                None => None,
                Some(Kind::Str(s)) if s.is_empty() => None,
                Some(Kind::Str(s)) => Some(budget.copy_string(s)?),
                Some(Kind::Ref(r)) => Some(budget.copy_string(&r.val)?),
                _ => return Err(ReadError::InvalidQuery("navId requires a string or Ref")),
            };
            ReadQuery::Nav(parent)
        }
        ReadOperation::Definitions => ReadQuery::Definitions {
            filter: optional_string(first.and_then(|r| r.get("filter")), budget)?,
        },
        ReadOperation::Libraries => ReadQuery::Libraries,
        ReadOperation::Specs => ReadQuery::Specs {
            library: optional_string(first.and_then(|r| r.get("lib")), budget)?,
        },
        ReadOperation::Spec => ReadQuery::Spec(
            optional_string(first.and_then(|r| r.get("qname")), budget)?
                .ok_or(ReadError::InvalidQuery("qname is required"))?,
        ),
    };
    let limit = grid
        .meta
        .get("limit")
        .or_else(|| first.and_then(|r| r.get("limit")));
    let page_size = match limit {
        None => budget.limits.max_rows.min(100),
        Some(Kind::Number(n))
            if n.unit.is_none()
                && n.val.is_finite()
                && n.val >= 1.0
                && n.val.fract() == 0.0
                && n.val <= budget.limits.max_rows as f64 =>
        {
            n.val as usize
        }
        _ => {
            return Err(ReadError::InvalidQuery(
                "limit must be a positive bounded integer",
            ));
        }
    };
    let cursor = optional_string(grid.meta.get("cursor"), budget)?;
    let mut projection = Vec::new();
    match grid.meta.get("select") {
        None => {}
        Some(Kind::List(tags)) => {
            for tag in tags {
                budget.charge(BudgetKind::Retained, 64)?;
                let Some(tag) = optional_string(Some(tag), budget)? else {
                    return Err(ReadError::InvalidQuery("empty projection tag"));
                };
                projection.push(tag);
            }
        }
        _ => {
            return Err(ReadError::InvalidQuery(
                "select must be a list of tag strings",
            ));
        }
    }
    Ok(ReadRequest {
        query,
        projection,
        page_size,
        cursor,
        profile: OutputProfile::H4(output),
    })
}
fn optional_string(value: Option<&Kind>, budget: &mut Budget) -> Result<Option<String>, ReadError> {
    match value {
        None => Ok(None),
        Some(Kind::Str(s)) if s.is_empty() => Ok(None),
        Some(Kind::Str(s)) => Ok(Some(budget.copy_string(s)?)),
        _ => Err(ReadError::InvalidQuery("expected string field")),
    }
}
