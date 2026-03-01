// Validation issue types for ontology and graph validation.

/// Describes why an entity does not fit a type.
#[derive(Debug, Clone, PartialEq)]
pub enum FitIssue {
    /// Entity is missing a mandatory marker tag.
    MissingMarker {
        /// The missing marker name.
        tag: String,
        /// The type that requires this marker.
        spec: String,
    },
    /// Tag value has wrong type.
    WrongType {
        /// The tag name.
        tag: String,
        /// Expected type description.
        expected: String,
        /// Actual type description.
        actual: String,
    },
    /// Ref tag points to wrong entity type.
    InvalidRef {
        /// The ref tag name.
        tag: String,
        /// Expected target type.
        expected: String,
        /// Actual target type or description.
        actual: String,
    },
    /// Entity is missing a required choice selection.
    MissingChoice {
        /// The choice tag name.
        tag: String,
        /// Valid options for this choice.
        options: Vec<String>,
    },
    /// A value constraint was violated (minVal, maxVal, pattern, etc.)
    ConstraintViolation {
        /// The tag name.
        tag: String,
        /// The constraint that was violated.
        constraint: String,
        /// Human-readable detail.
        detail: String,
    },
    /// A choice slot has an invalid value.
    InvalidChoice {
        /// The tag name.
        tag: String,
        /// The invalid value.
        value: String,
        /// Valid options.
        valid_options: Vec<String>,
    },
}

/// A validation problem found in an entity or graph.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidationIssue {
    /// The entity ref val, or `None` for schema-level issues.
    pub entity: Option<String>,
    /// Category such as `"missing_marker"` or `"invalid_ref"`.
    pub issue_type: String,
    /// Human-readable description of the problem.
    pub detail: String,
}

impl std::fmt::Display for FitIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FitIssue::MissingMarker { tag, spec } => {
                write!(f, "missing mandatory marker '{tag}' for spec '{spec}'")
            }
            FitIssue::WrongType { tag, expected, actual } => {
                write!(f, "wrong type for '{tag}': expected {expected}, got {actual}")
            }
            FitIssue::InvalidRef { tag, expected, actual } => {
                write!(f, "invalid ref for '{tag}': expected {expected}, got {actual}")
            }
            FitIssue::MissingChoice { tag, options } => {
                write!(f, "missing choice for '{tag}': options are {}", options.join(", "))
            }
            FitIssue::ConstraintViolation { tag, constraint, detail } => {
                write!(f, "constraint '{constraint}' violated on '{tag}': {detail}")
            }
            FitIssue::InvalidChoice { tag, value, valid_options } => {
                write!(f, "invalid choice for '{tag}': '{}' not in [{}]", value, valid_options.join(", "))
            }
        }
    }
}

impl std::fmt::Display for ValidationIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}: {}", self.issue_type, self.entity.as_deref().unwrap_or("?"), self.detail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_issue_missing_marker() {
        let issue = FitIssue::MissingMarker {
            tag: "equip".to_string(),
            spec: "ahu".to_string(),
        };
        match &issue {
            FitIssue::MissingMarker { tag, spec } => {
                assert_eq!(tag, "equip");
                assert_eq!(spec, "ahu");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn fit_issue_equality() {
        let a = FitIssue::MissingMarker {
            tag: "equip".to_string(),
            spec: "ahu".to_string(),
        };
        let b = FitIssue::MissingMarker {
            tag: "equip".to_string(),
            spec: "ahu".to_string(),
        };
        assert_eq!(a, b);
    }

    #[test]
    fn validation_issue() {
        let issue = ValidationIssue {
            entity: Some("site-1".to_string()),
            issue_type: "missing_marker".to_string(),
            detail: "Entity claims 'ahu' but is missing mandatory marker 'equip'"
                .to_string(),
        };
        assert_eq!(issue.entity, Some("site-1".to_string()));
        assert_eq!(issue.issue_type, "missing_marker");
    }
}
