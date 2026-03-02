// Haystack Grid — a two-dimensional tagged data structure.

use super::dict::HDict;
use std::fmt;
use std::sync::Arc;

/// A single column in a Haystack Grid.
///
/// Each column has a name and optional metadata dict.
#[derive(Debug, Clone, PartialEq)]
pub struct HCol {
    pub name: String,
    pub meta: HDict,
}

impl HCol {
    /// Create a column with just a name and empty metadata.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            meta: HDict::new(),
        }
    }

    /// Create a column with a name and metadata dict.
    pub fn with_meta(name: impl Into<String>, meta: HDict) -> Self {
        Self {
            name: name.into(),
            meta,
        }
    }
}

/// Haystack Grid — the fundamental tabular data structure.
///
/// A grid has:
/// - `meta`: grid-level metadata (an `HDict`)
/// - `cols`: ordered list of columns (`HCol`)
/// - `rows`: ordered list of row dicts (`HDict`)
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HGrid {
    pub meta: HDict,
    pub cols: Vec<HCol>,
    pub rows: Vec<HDict>,
}

impl HGrid {
    /// Create an empty grid.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a grid from its constituent parts.
    pub fn from_parts(meta: HDict, cols: Vec<HCol>, rows: Vec<HDict>) -> Self {
        Self { meta, cols, rows }
    }

    /// Build a grid from Arc-wrapped rows, avoiding clones when possible.
    ///
    /// Uses `Arc::try_unwrap()` to move the inner HDict when the reference count
    /// is 1 (which is the common case in request pipelines). Falls back to clone
    /// only for shared references.
    pub fn from_parts_arc(meta: HDict, cols: Vec<HCol>, rows: Vec<Arc<HDict>>) -> Self {
        let owned_rows: Vec<HDict> = rows
            .into_iter()
            .map(|arc| Arc::try_unwrap(arc).unwrap_or_else(|a| (*a).clone()))
            .collect();
        Self {
            meta,
            cols,
            rows: owned_rows,
        }
    }

    /// Look up a column by name. Returns `None` if not found.
    pub fn col(&self, name: &str) -> Option<&HCol> {
        self.cols.iter().find(|c| c.name == name)
    }

    /// Returns `true` if the grid has no rows.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Returns `true` if this grid represents an error response.
    ///
    /// An error grid has an `err` marker tag in its metadata.
    pub fn is_err(&self) -> bool {
        self.meta.has("err")
    }

    /// Returns the number of rows.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Returns a reference to the row at the given index.
    pub fn row(&self, index: usize) -> Option<&HDict> {
        self.rows.get(index)
    }

    /// Iterate over rows.
    pub fn iter(&self) -> impl Iterator<Item = &HDict> {
        self.rows.iter()
    }

    /// Returns the number of columns.
    pub fn num_cols(&self) -> usize {
        self.cols.len()
    }

    /// Returns an iterator over column names.
    pub fn col_names(&self) -> impl Iterator<Item = &str> {
        self.cols.iter().map(|c| c.name.as_str())
    }
}

impl fmt::Display for HGrid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HGrid(cols: [")?;
        for (i, col) in self.cols.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{}", col.name)?;
        }
        write!(f, "], rows: {})", self.rows.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kinds::{Kind, Number};

    fn sample_grid() -> HGrid {
        let cols = vec![HCol::new("id"), HCol::new("dis"), HCol::new("area")];

        let mut row1 = HDict::new();
        row1.set("id", Kind::Ref(crate::kinds::HRef::from_val("site-1")));
        row1.set("dis", Kind::Str("Site One".into()));
        row1.set(
            "area",
            Kind::Number(Number::new(4500.0, Some("ft\u{00B2}".into()))),
        );

        let mut row2 = HDict::new();
        row2.set("id", Kind::Ref(crate::kinds::HRef::from_val("site-2")));
        row2.set("dis", Kind::Str("Site Two".into()));
        row2.set(
            "area",
            Kind::Number(Number::new(3200.0, Some("ft\u{00B2}".into()))),
        );

        HGrid::from_parts(HDict::new(), cols, vec![row1, row2])
    }

    #[test]
    fn empty_grid() {
        let g = HGrid::new();
        assert!(g.is_empty());
        assert_eq!(g.len(), 0);
        assert_eq!(g.num_cols(), 0);
        assert!(!g.is_err());
        assert_eq!(g.row(0), None);
    }

    #[test]
    fn grid_with_data() {
        let g = sample_grid();
        assert!(!g.is_empty());
        assert_eq!(g.len(), 2);
        assert_eq!(g.num_cols(), 3);
    }

    #[test]
    fn col_lookup() {
        let g = sample_grid();

        let id_col = g.col("id").unwrap();
        assert_eq!(id_col.name, "id");

        let dis_col = g.col("dis").unwrap();
        assert_eq!(dis_col.name, "dis");

        assert!(g.col("nonexistent").is_none());
    }

    #[test]
    fn col_names() {
        let g = sample_grid();
        let names: Vec<&str> = g.col_names().collect();
        assert_eq!(names, vec!["id", "dis", "area"]);
    }

    #[test]
    fn row_access() {
        let g = sample_grid();

        let r0 = g.row(0).unwrap();
        assert_eq!(r0.get("dis"), Some(&Kind::Str("Site One".into())));

        let r1 = g.row(1).unwrap();
        assert_eq!(r1.get("dis"), Some(&Kind::Str("Site Two".into())));

        assert!(g.row(2).is_none());
    }

    #[test]
    fn iteration() {
        let g = sample_grid();
        let rows: Vec<&HDict> = g.iter().collect();
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn is_err_false_for_normal_grid() {
        let g = sample_grid();
        assert!(!g.is_err());
    }

    #[test]
    fn is_err_true_with_err_marker() {
        let mut meta = HDict::new();
        meta.set("err", Kind::Marker);
        meta.set("dis", Kind::Str("some error message".into()));

        let g = HGrid::from_parts(meta, vec![], vec![]);
        assert!(g.is_err());
        assert!(g.is_empty());
    }

    #[test]
    fn col_with_meta() {
        let mut meta = HDict::new();
        meta.set("unit", Kind::Str("kW".into()));

        let col = HCol::with_meta("power", meta);
        assert_eq!(col.name, "power");
        assert!(col.meta.has("unit"));
    }

    #[test]
    fn display() {
        let g = sample_grid();
        let s = g.to_string();
        assert!(s.contains("id"));
        assert!(s.contains("dis"));
        assert!(s.contains("area"));
        assert!(s.contains("rows: 2"));
    }

    #[test]
    fn equality() {
        let a = sample_grid();
        let b = sample_grid();
        assert_eq!(a, b);
    }

    #[test]
    fn default_is_empty() {
        let g = HGrid::default();
        assert!(g.is_empty());
        assert_eq!(g.num_cols(), 0);
    }

    #[test]
    fn from_parts() {
        let cols = vec![HCol::new("name")];
        let mut row = HDict::new();
        row.set("name", Kind::Str("test".into()));

        let g = HGrid::from_parts(HDict::new(), cols, vec![row]);
        assert_eq!(g.len(), 1);
        assert_eq!(g.num_cols(), 1);
    }
}
