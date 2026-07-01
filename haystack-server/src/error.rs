//! Haystack error grids and Axum error integration.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use std::fmt;

use haystack_core::data::{HCol, HDict, HGrid};
use haystack_core::kinds::Kind;

use crate::content;

/// Build a Haystack error grid with the given message.
///
/// An error grid has `err` marker and `dis` string in its metadata,
/// with no columns and no rows.
pub fn error_grid(message: &str) -> HGrid {
    let mut meta = HDict::new();
    meta.set("err", Kind::Marker);
    meta.set("dis", Kind::Str(message.to_string()));
    HGrid::from_parts(meta, vec![], vec![])
}

/// Haystack-specific error type that renders as an error grid in responses.
#[derive(Debug)]
pub struct HaystackError {
    pub message: String,
    pub status: StatusCode,
}

impl HaystackError {
    pub fn new(message: impl Into<String>, status: StatusCode) -> Self {
        Self {
            message: message.into(),
            status,
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(message, StatusCode::BAD_REQUEST)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(message, StatusCode::NOT_FOUND)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(message, StatusCode::INTERNAL_SERVER_ERROR)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(message, StatusCode::FORBIDDEN)
    }
}

impl fmt::Display for HaystackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

// Note: Error responses are always encoded as Zinc (the default Haystack format)
// regardless of the client's Accept header, because IntoResponse does not have
// access to the original request. All Haystack clients must support Zinc decoding.
impl IntoResponse for HaystackError {
    fn into_response(self) -> Response {
        let grid = error_grid(&self.message);
        match content::encode_response_grid(&grid, "text/zinc") {
            Ok((body, content_type)) => (
                self.status,
                [(axum::http::header::CONTENT_TYPE, content_type)],
                body,
            )
                .into_response(),
            Err(_) => (
                self.status,
                [(axum::http::header::CONTENT_TYPE, "text/plain")],
                format!("Error: {}", self.message),
            )
                .into_response(),
        }
    }
}

/// Helper to create an HGrid with a single empty row and named columns.
///
/// Used by op handlers that return simple tabular data.
pub fn grid_from_cols_rows(col_names: &[&str], rows: Vec<HDict>) -> HGrid {
    let cols: Vec<HCol> = col_names.iter().map(|n| HCol::new(*n)).collect();
    HGrid::from_parts(HDict::new(), cols, rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_grid_has_err_marker() {
        let grid = error_grid("something went wrong");
        assert!(grid.is_err());
        assert_eq!(
            grid.meta.get("dis"),
            Some(&Kind::Str("something went wrong".to_string()))
        );
        assert!(grid.is_empty());
    }

    #[test]
    fn haystack_error_display() {
        let err = HaystackError::bad_request("invalid filter");
        assert_eq!(err.to_string(), "invalid filter");
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn grid_from_cols_rows_builds_correctly() {
        let mut row = HDict::new();
        row.set("name", Kind::Str("about".into()));
        row.set("summary", Kind::Str("Summary of about op".into()));

        let grid = grid_from_cols_rows(&["name", "summary"], vec![row]);
        assert_eq!(grid.len(), 1);
        assert_eq!(grid.cols.len(), 2);
    }
}
