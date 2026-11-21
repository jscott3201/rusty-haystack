//! Shared strict point schema. Write timestamp spelling is deliberately checked
//! separately from read-range conversion.
use crate::{ReadError, budget::Budget, history_range::validate_zone};
use haystack_core::{
    codecs::history::{HistoryKind, HistorySchema},
    data::HDict,
    kinds::{Kind, unit_for},
};
pub(crate) fn schema(row: &HDict, budget: &mut Budget) -> Result<HistorySchema, ReadError> {
    if !matches!(row.get("his"), Some(Kind::Marker)) {
        return Err(ReadError::InvalidQuery("point does not declare history"));
    }
    let kind = match row.get("kind") {
        Some(Kind::Str(value)) if value == "Bool" => HistoryKind::Bool,
        Some(Kind::Str(value)) if value == "Number" => HistoryKind::Number,
        Some(Kind::Str(value)) if value == "Str" => HistoryKind::Str,
        _ => return Err(ReadError::Projection),
    };
    let Some(Kind::Str(timezone)) = row.get("tz") else {
        return Err(ReadError::InvalidQuery("point requires history timezone"));
    };
    if timezone.len() > 128 {
        return Err(ReadError::InvalidQuery("invalid history timezone"));
    }
    validate_zone(timezone)?;
    let unit = match row.get("unit") {
        None if kind != HistoryKind::Number => None,
        Some(Kind::Str(unit))
            if kind == HistoryKind::Number && unit.len() <= 128 && unit_for(unit).is_some() =>
        {
            Some(budget.copy_string(unit)?)
        }
        _ => return Err(ReadError::Projection),
    };
    Ok(HistorySchema {
        kind,
        unit,
        timezone: budget.copy_string(timezone)?,
    })
}
