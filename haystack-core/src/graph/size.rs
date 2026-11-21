//! Conservative bounded work/retained-value accounting before cloning records.
use crate::{data::HDict, kinds::Kind};

pub(crate) struct ValueBudget {
    work_left: usize,
    bytes_left: usize,
    initial_bytes: usize,
    depth: usize,
}
impl ValueBudget {
    pub(crate) fn new(work: usize, bytes: usize, depth: usize) -> Self {
        Self {
            work_left: work,
            bytes_left: bytes,
            initial_bytes: bytes,
            depth,
        }
    }
    pub(crate) fn charge(&mut self, work: usize, bytes: usize) -> Result<(), ()> {
        if work > self.work_left || bytes > self.bytes_left {
            return Err(());
        }
        self.work_left -= work;
        self.bytes_left -= bytes;
        Ok(())
    }
    pub(crate) fn used_bytes(&self) -> usize {
        self.initial_bytes - self.bytes_left
    }
    pub(crate) fn dict(&mut self, dict: &HDict, depth: usize) -> Result<(), ()> {
        if depth > self.depth {
            return Err(());
        }
        self.charge(1, 128)?;
        for (name, value) in dict.iter() {
            self.charge(name.len().saturating_add(1), name.len().saturating_add(64))?;
            self.value(value, depth + 1)?;
        }
        Ok(())
    }
    pub(crate) fn value(&mut self, value: &Kind, depth: usize) -> Result<(), ()> {
        if depth > self.depth {
            return Err(());
        }
        let bytes = match value {
            Kind::Str(v) => v.len(),
            Kind::Number(v) => v.unit.as_ref().map_or(0, String::len),
            Kind::Ref(v) => v
                .val
                .len()
                .saturating_add(v.dis.as_ref().map_or(0, String::len)),
            Kind::Uri(v) => v.val().len(),
            Kind::Symbol(v) => v.val().len(),
            Kind::DateTime(v) => v.tz_name.len(),
            Kind::XStr(v) => v.type_name.len().saturating_add(v.val.len()),
            Kind::Buf(v) => v.len(),
            Kind::Nominal(v) => v
                .spec()
                .len()
                .saturating_add(v.catalog().len())
                .saturating_add(v.revision().len())
                .saturating_add(v.text().len()),
            _ => 0,
        };
        self.charge(bytes.saturating_add(1), bytes.saturating_add(512))?;
        match value {
            Kind::List(items) => {
                for item in items {
                    self.value(item, depth + 1)?;
                }
            }
            Kind::Dict(dict) => self.dict(dict, depth + 1)?,
            Kind::Grid(grid) => {
                self.dict(&grid.meta, depth + 1)?;
                for column in &grid.cols {
                    self.charge(
                        column.name.len().saturating_add(1),
                        column.name.len().saturating_add(64),
                    )?;
                    self.dict(&column.meta, depth + 1)?;
                }
                for row in &grid.rows {
                    self.dict(row, depth + 1)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

pub(crate) fn diff_bytes<'a>(
    id: &str,
    rows: impl IntoIterator<Item = &'a HDict>,
    limit: usize,
) -> Option<usize> {
    let mut budget = ValueBudget::new(usize::MAX, limit, 64);
    budget.charge(1, id.len().saturating_add(256)).ok()?;
    for row in rows {
        budget.dict(row, 1).ok()?;
    }
    Some(budget.used_bytes())
}

pub(crate) fn patch_diff_bytes(
    id: &str,
    old: &HDict,
    changes: &HDict,
    limit: usize,
) -> Option<usize> {
    let mut budget = ValueBudget::new(usize::MAX, limit, 64);
    budget.charge(1, id.len().saturating_add(384)).ok()?;
    budget.dict(changes, 1).ok()?;
    for (name, _) in changes.iter() {
        if let Some(value) = old.get(name) {
            budget
                .charge(name.len().saturating_add(1), name.len().saturating_add(64))
                .ok()?;
            budget.value(value, 2).ok()?;
        }
    }
    Some(budget.used_bytes())
}
