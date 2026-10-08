//! Explicit projection into the H4 *value model*. This is not a claim that a
//! particular H4 codec roundtrips the result: text numbers can lose NaN bits or
//! signed zero; grids can lose missing/null distinctions, columns or metadata.
use crate::data::{HCol, HDict, HGrid};
use crate::kinds::{Kind, Number};

/// A location in the original value, independent of a wire format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValuePathSegment {
    Tag(String),
    Item(usize),
    GridMeta,
    ColumnMeta(usize),
    Row(usize),
}
pub type ValuePath = Vec<ValuePathSegment>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionReason {
    IntTypeErased,
    IntPrecisionLost,
    FloatTypeErased,
    NoneBecameNull,
    BufUnsupported,
    NominalUnsupported,
    NestingLimit,
}
impl ProjectionReason {
    fn unsupported(&self) -> bool {
        matches!(
            self,
            Self::BufUnsupported | Self::NominalUnsupported | Self::NestingLimit
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionIssue {
    pub path: ValuePath,
    pub reason: ProjectionReason,
}

/// Policy applies to semantic losses only. Codec-specific fidelity must still
/// be checked by the caller; CSV, for example, has no grid decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionPolicy {
    Strict,
    AllowLoss,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum H4Projection {
    Exact(Kind),
    Lossy {
        value: Kind,
        issues: Vec<ProjectionIssue>,
    },
    Unsupported {
        issues: Vec<ProjectionIssue>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("H4 semantic projection rejected: {issues:?}")]
pub struct ProjectionError {
    pub issues: Vec<ProjectionIssue>,
}

impl H4Projection {
    /// Consume an assessed projection. Strict rejects every loss; unsupported
    /// values are rejected under both policies, with no partial result.
    pub fn into_value(self, policy: ProjectionPolicy) -> Result<Kind, ProjectionError> {
        match self {
            Self::Exact(value) => Ok(value),
            Self::Lossy { value, .. } if policy == ProjectionPolicy::AllowLoss => Ok(value),
            Self::Lossy { issues, .. } | Self::Unsupported { issues } => {
                Err(ProjectionError { issues })
            }
        }
    }
}

impl Kind {
    /// Assess a recursive H4 semantic projection without changing the source.
    /// Recursion beyond 64 levels is unsupported. Exact means that the H4
    /// value model can retain this value, not that every H4 codec can do so.
    pub fn project_h4(&self) -> H4Projection {
        let mut issues = Vec::new();
        let value = project(self, &mut Vec::new(), &mut issues, 0);
        if issues.iter().any(|issue| issue.reason.unsupported()) {
            H4Projection::Unsupported { issues }
        } else if issues.is_empty() {
            H4Projection::Exact(value)
        } else {
            H4Projection::Lossy { value, issues }
        }
    }
}

fn at<T>(
    path: &mut ValuePath,
    segment: ValuePathSegment,
    f: impl FnOnce(&mut ValuePath) -> T,
) -> T {
    path.push(segment);
    let result = f(path);
    path.pop();
    result
}
fn project_dict(
    dict: &HDict,
    path: &mut ValuePath,
    issues: &mut Vec<ProjectionIssue>,
    depth: usize,
) -> HDict {
    let mut result = HDict::new();
    for (tag, value) in dict.sorted_tags() {
        let value = at(path, ValuePathSegment::Tag(tag.into()), |path| {
            project(value, path, issues, depth + 1)
        });
        result.set(tag, value);
    }
    result
}
fn project(
    value: &Kind,
    path: &mut ValuePath,
    issues: &mut Vec<ProjectionIssue>,
    depth: usize,
) -> Kind {
    let mut issue = |reason| {
        issues.push(ProjectionIssue {
            path: path.clone(),
            reason,
        })
    };
    if depth > 64 {
        issue(ProjectionReason::NestingLimit);
        return Kind::Null;
    }
    match value {
        Kind::Int(v) => {
            issue(ProjectionReason::IntTypeErased);
            // i128 avoids saturating i64 casts hiding rounding at i64::MAX.
            if (*v as f64) as i128 != i128::from(*v) {
                issue(ProjectionReason::IntPrecisionLost);
            }
            Kind::Number(Number::unitless(*v as f64))
        }
        Kind::Float(v) => {
            issue(ProjectionReason::FloatTypeErased);
            Kind::Number(Number::unitless(v.value()))
        }
        Kind::None => {
            issue(ProjectionReason::NoneBecameNull);
            Kind::Null
        }
        Kind::Buf(_) => {
            issue(ProjectionReason::BufUnsupported);
            Kind::Null
        }
        Kind::Nominal(_) => {
            issue(ProjectionReason::NominalUnsupported);
            Kind::Null
        }
        Kind::List(values) => Kind::List(
            values
                .iter()
                .enumerate()
                .map(|(i, value)| {
                    at(path, ValuePathSegment::Item(i), |path| {
                        project(value, path, issues, depth + 1)
                    })
                })
                .collect(),
        ),
        Kind::Dict(dict) => Kind::Dict(Box::new(project_dict(dict, path, issues, depth))),
        Kind::Grid(grid) => {
            let meta = at(path, ValuePathSegment::GridMeta, |path| {
                project_dict(&grid.meta, path, issues, depth + 1)
            });
            let cols = grid
                .cols
                .iter()
                .enumerate()
                .map(|(i, col)| {
                    HCol::with_meta(
                        &col.name,
                        at(path, ValuePathSegment::ColumnMeta(i), |path| {
                            project_dict(&col.meta, path, issues, depth + 1)
                        }),
                    )
                })
                .collect();
            let rows = grid
                .rows
                .iter()
                .enumerate()
                .map(|(i, row)| {
                    at(path, ValuePathSegment::Row(i), |path| {
                        project_dict(row, path, issues, depth + 1)
                    })
                })
                .collect();
            Kind::Grid(Box::new(HGrid::from_parts(meta, cols, rows)))
        }
        value => value.clone(),
    }
}

/// Validate the context-free H4 boundary without projecting or dropping values.
/// Also examines metadata and row tags which some H4 encoders would discard.
pub fn ensure_h4(value: &Kind) -> Result<(), crate::codecs::CodecError> {
    visit(value, &mut Vec::new(), 0)
}
pub fn ensure_h4_dict(dict: &HDict) -> Result<(), crate::codecs::CodecError> {
    visit_dict(dict, &mut Vec::new(), 0)
}
pub fn ensure_h4_grid(grid: &HGrid) -> Result<(), crate::codecs::CodecError> {
    visit_grid(grid, &mut Vec::new(), 0)
}
fn visit_dict(
    dict: &HDict,
    path: &mut ValuePath,
    depth: usize,
) -> Result<(), crate::codecs::CodecError> {
    for (tag, value) in dict.sorted_tags() {
        at(path, ValuePathSegment::Tag(tag.into()), |path| {
            visit(value, path, depth + 1)
        })?;
    }
    Ok(())
}
fn visit_grid(
    grid: &HGrid,
    path: &mut ValuePath,
    depth: usize,
) -> Result<(), crate::codecs::CodecError> {
    at(path, ValuePathSegment::GridMeta, |path| {
        visit_dict(&grid.meta, path, depth + 1)
    })?;
    for (i, col) in grid.cols.iter().enumerate() {
        at(path, ValuePathSegment::ColumnMeta(i), |path| {
            visit_dict(&col.meta, path, depth + 1)
        })?;
    }
    for (i, row) in grid.rows.iter().enumerate() {
        at(path, ValuePathSegment::Row(i), |path| {
            visit_dict(row, path, depth + 1)
        })?;
    }
    Ok(())
}
fn visit(
    value: &Kind,
    path: &mut ValuePath,
    depth: usize,
) -> Result<(), crate::codecs::CodecError> {
    use crate::codecs::CodecError;
    if depth > 64 {
        return Err(CodecError::Encode(format!(
            "value nesting exceeds 64 at {path:?}"
        )));
    }
    match value {
        Kind::Int(_) | Kind::Float(_) | Kind::None | Kind::Buf(_) | Kind::Nominal(_) => {
            Err(CodecError::Unprojected { path: path.clone() })
        }
        Kind::List(values) => {
            for (i, value) in values.iter().enumerate() {
                at(path, ValuePathSegment::Item(i), |path| {
                    visit(value, path, depth + 1)
                })?;
            }
            Ok(())
        }
        Kind::Dict(dict) => visit_dict(dict, path, depth),
        Kind::Grid(grid) => visit_grid(grid, path, depth),
        _ => Ok(()),
    }
}
