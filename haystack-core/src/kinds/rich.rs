//! Typed scalars. These are semantic values, not implicit H4 wire conversions.

/// Binary64 representation identity. Every bit pattern is retained, including
/// signed zero and NaN payloads. Numeric comparison uses [`Self::value`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Float(u64);

impl Float {
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }
    pub const fn new(value: f64) -> Self {
        Self(value.to_bits())
    }
    pub const fn bits(self) -> u64 {
        self.0
    }
    pub const fn value(self) -> f64 {
        f64::from_bits(self.0)
    }
}

/// A nominal scalar records its source catalog identity and exact scalar text.
/// This does not assert that the catalog exists or that the value fits its spec.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NominalScalar {
    spec: String,
    catalog: String,
    revision: String,
    text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("nominal scalar requires a qualified library::Spec and nonempty catalog identity/revision")]
pub struct NominalError;

impl NominalScalar {
    pub fn new(
        spec: impl Into<String>,
        catalog: impl Into<String>,
        revision: impl Into<String>,
        text: impl Into<String>,
    ) -> Result<Self, NominalError> {
        let value = Self {
            spec: spec.into(),
            catalog: catalog.into(),
            revision: revision.into(),
            text: text.into(),
        };
        let qualified = value.spec.split_once("::").is_some_and(|(lib, name)| {
            !lib.is_empty()
                && !name.is_empty()
                && !name.contains(':')
                && !lib.contains(':')
                && !value.spec.chars().any(char::is_whitespace)
        });
        if !qualified || value.catalog.is_empty() || value.revision.is_empty() {
            return Err(NominalError);
        }
        Ok(value)
    }
    pub fn spec(&self) -> &str {
        &self.spec
    }
    pub fn catalog(&self) -> &str {
        &self.catalog
    }
    pub fn revision(&self) -> &str {
        &self.revision
    }
    pub fn text(&self) -> &str {
        &self.text
    }
}
