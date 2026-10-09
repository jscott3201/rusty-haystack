use regex_automata::{
    Anchored, Input, MatchError,
    dfa::{Automaton, StartKind, dense},
    nfa::thompson,
    util::syntax,
};
use std::sync::Arc;
/// Admitted tables are immutable and shared; searches use stack state only.
#[derive(Debug, Clone)]
pub(super) struct Pattern(Arc<dense::DFA<Vec<u32>>>);
impl Pattern {
    fn new(source: &str) -> Result<Self, ContextError> {
        let dfa = dense::Builder::new()
            .configure(
                dense::Config::new()
                    .start_kind(StartKind::Anchored)
                    .unicode_word_boundary(false)
                    .dfa_size_limit(Some(64 * 1024))
                    .determinize_size_limit(Some(256 * 1024)),
            )
            .syntax(syntax::Config::new().nest_limit(32))
            .thompson(thompson::Config::new().nfa_size_limit(Some(64 * 1024)))
            .build(&format!("\\A(?:{source})\\z"))
            .map_err(|_| ContextError("invalid, unsupported or oversized pattern"))?;
        if dfa.memory_usage() > 64 * 1024 {
            return Err(ContextError("pattern table bound"));
        }
        Ok(Self(Arc::new(dfa)))
    }
    pub(super) fn is_match(&self, text: &str) -> Result<bool, MatchError> {
        self.0
            .try_search_fwd(&Input::new(text).anchored(Anchored::Yes))
            .map(|found| found.is_some())
    }
}
use std::collections::BTreeMap;
/// Small codec declarations, already resolved to qualified names. No query,
/// fitting, registry execution or implicit library loading is performed here.
#[derive(Debug, Clone)]
pub enum Definition {
    Nominal {
        name: String,
        pattern: String,
    },
    /// Finite scalar keys. Strings decode with this enum's nominal identity.
    Enum {
        name: String,
        keys: Vec<String>,
    },
    Dict {
        name: String,
        members: BTreeMap<String, String>,
    },
    List {
        name: String,
        of: String,
    },
    Grid {
        name: String,
        of: Option<String>,
    },
    /// Ref subtyping classifies relationships without changing value identity.
    Ref {
        name: String,
    },
}
impl Definition {
    fn name(&self) -> &str {
        match self {
            Self::Nominal { name, .. }
            | Self::Enum { name, .. }
            | Self::Dict { name, .. }
            | Self::List { name, .. }
            | Self::Grid { name, .. }
            | Self::Ref { name } => name,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Scalar {
    Bool,
    Int,
    Float,
    Number,
    Str,
    Ref,
    Marker,
    None,
    NA,
    Buf,
    Uri,
    Date,
    Time,
    DateTime,
}
#[derive(Debug, Clone)]
pub(super) enum Class {
    Any,
    Scalar(Scalar),
    Nominal(Pattern),
    Enum(Arc<[String]>),
    Dict(BTreeMap<String, String>),
    List(Option<String>),
    Grid(Option<String>),
}
impl Class {
    pub(super) fn scalar(&self) -> bool {
        matches!(self, Self::Scalar(_) | Self::Nominal(_) | Self::Enum(_))
    }
}
/// Immutable, closed context. Nominals carry this explicit catalog identity and
/// revision. Identifiers are supplied by the owner of the admitted artifact;
/// construction validates this bounded codec schema, not remote provenance.
#[derive(Debug, Clone)]
pub struct Context {
    catalog: String,
    revision: String,
    types: BTreeMap<String, Class>,
    max_enum_keys: usize,
}
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("invalid Jeto context: {0}")]
pub struct ContextError(pub &'static str);
impl Context {
    /// Built-in native scalar/container subset of the pinned Jeto contract.
    /// This does not admit arbitrary sys names or nominal domain libraries.
    pub fn standard() -> Self {
        use Scalar::*;
        let mut types = BTreeMap::new();
        for (name, scalar) in [
            ("Bool", Bool),
            ("Int", Int),
            ("Float", Float),
            ("Number", Number),
            ("Str", Str),
            ("Ref", Ref),
            ("Marker", Marker),
            ("None", None),
            ("NA", NA),
            ("Buf", Buf),
            ("Uri", Uri),
            ("Date", Date),
            ("Time", Time),
            ("DateTime", DateTime),
        ] {
            types.insert(format!("sys::{name}"), Class::Scalar(scalar));
        }
        types.insert("sys::Obj".into(), Class::Any);
        types.insert("sys::Dict".into(), Class::Dict(BTreeMap::new()));
        types.insert("sys::List".into(), Class::List(Option::None));
        types.insert("sys::Grid".into(), Class::Grid(Option::None));
        Self {
            catalog: "project-haystack/xeto".into(),
            revision: crate::xeto::catalog::PINNED_XETO_REVISION.into(),
            types,
            max_enum_keys: 0,
        }
    }
    /// Add a bounded set of caller-admitted codec declarations. Duplicate names,
    /// missing references and unsupported pattern complexity are rejected.
    pub fn new(
        catalog: impl Into<String>,
        revision: impl Into<String>,
        definitions: Vec<Definition>,
    ) -> Result<Self, ContextError> {
        let mut context = Self::standard();
        context.catalog = catalog.into();
        context.revision = revision.into();
        if context.catalog.is_empty()
            || context.revision.is_empty()
            || context.catalog.len() > 256
            || context.revision.len() > 256
            || definitions.len() > 256
        {
            return Err(ContextError("identity or declaration bound"));
        }
        let mut source_bytes = 0usize;
        for definition in definitions {
            let name = definition.name();
            if !qname(name) || context.types.contains_key(name) {
                return Err(ContextError("duplicate or invalid qualified name"));
            }
            source_bytes = source_bytes.saturating_add(name.len());
            let class = match &definition {
                Definition::Nominal { pattern, .. } => {
                    if pattern.len() > 1024 {
                        return Err(ContextError("pattern source bound"));
                    }
                    source_bytes = source_bytes.saturating_add(pattern.len());
                    let regex = Pattern::new(pattern)?;
                    Class::Nominal(regex)
                }
                Definition::Enum { keys, .. } => {
                    if keys.is_empty()
                        || keys.len() > 4096
                        || keys.iter().any(|key| key.is_empty() || key.len() > 256)
                    {
                        return Err(ContextError("enum member bound"));
                    }
                    let bytes = keys
                        .iter()
                        .try_fold(0usize, |n, key| n.checked_add(key.len()))
                        .ok_or(ContextError("enum source bound"))?;
                    if bytes > 64 * 1024 {
                        return Err(ContextError("enum source bound"));
                    }
                    source_bytes = source_bytes.saturating_add(bytes);
                    if source_bytes > 128 * 1024 {
                        return Err(ContextError("context source bound"));
                    }
                    let mut keys = keys.clone();
                    keys.sort();
                    if keys.windows(2).any(|pair| pair[0] == pair[1]) {
                        return Err(ContextError("duplicate enum key"));
                    }
                    context.max_enum_keys = context.max_enum_keys.max(keys.len());
                    Class::Enum(keys.into())
                }
                Definition::Dict { members, .. } => {
                    if members.len() > 256
                        || members
                            .keys()
                            .any(|k| k.is_empty() || k.len() > 256 || k == "spec")
                        || members.values().any(|v| !qname(v))
                    {
                        return Err(ContextError("member bound or reserved spec member"));
                    }
                    source_bytes = source_bytes.saturating_add(
                        members
                            .iter()
                            .map(|(k, v)| k.len().saturating_add(v.len()))
                            .sum::<usize>(),
                    );
                    if source_bytes > 128 * 1024 {
                        return Err(ContextError("context source bound"));
                    }
                    Class::Dict(members.clone())
                }
                Definition::List { of, .. } => {
                    if !qname(of) {
                        return Err(ContextError("invalid element type"));
                    }
                    source_bytes = source_bytes.saturating_add(of.len());
                    Class::List(Some(of.clone()))
                }
                Definition::Grid { of, .. } => {
                    if of.as_ref().is_some_and(|of| !qname(of)) {
                        return Err(ContextError("invalid row type"));
                    }
                    source_bytes = source_bytes.saturating_add(of.as_ref().map_or(0, String::len));
                    Class::Grid(of.clone())
                }
                Definition::Ref { .. } => Class::Scalar(Scalar::Ref),
            };
            if source_bytes > 128 * 1024 {
                return Err(ContextError("context source bound"));
            }
            context.types.insert(name.to_owned(), class);
        }
        for class in context.types.values() {
            match class {
                Class::Dict(members)
                    if members
                        .values()
                        .any(|name| !context.types.contains_key(name)) =>
                {
                    return Err(ContextError("unresolved member type"));
                }
                Class::List(Some(of)) if !context.types.contains_key(of) => {
                    return Err(ContextError("unresolved element type"));
                }
                Class::Grid(Some(of)) if !matches!(context.types.get(of), Some(Class::Dict(_))) => {
                    return Err(ContextError("grid row type must be an admitted Dict"));
                }
                _ => {}
            }
        }
        Ok(context)
    }
    pub fn catalog(&self) -> &str {
        &self.catalog
    }
    pub fn revision(&self) -> &str {
        &self.revision
    }
    pub fn contains(&self, name: &str) -> bool {
        self.types.contains_key(name)
    }
    // Binary search uses no allocation and at most log2(n)+2 comparisons.
    // Reserve their worst-case common-prefix scans before scalar membership.
    pub(super) fn enum_work(&self, bytes: usize) -> usize {
        if self.max_enum_keys == 0 {
            return 0;
        }
        bytes
            .saturating_add(1)
            .saturating_mul(self.max_enum_keys.ilog2() as usize + 2)
    }
    pub(super) fn lookup(&self, name: &str) -> Option<&Class> {
        self.types.get(name)
    }
}
fn qname(name: &str) -> bool {
    name.len() <= 256
        && name.split_once("::").is_some_and(|(lib, ty)| {
            !lib.is_empty()
                && !ty.is_empty()
                && !lib.contains(':')
                && !ty.contains(':')
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_:.".contains(&b))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codecs::jeto::{self, Boxing, Error, Limits};
    use crate::kinds::{Kind, NominalScalar};
    #[test]
    fn unexpected_matcher_error_is_not_a_membership_result() {
        // Production admission disables heuristic Unicode boundaries. Inject
        // such a DFA to exercise the defensive error path on non-ASCII input.
        let dfa = dense::Builder::new()
            .configure(
                dense::Config::new()
                    .start_kind(StartKind::Anchored)
                    .unicode_word_boundary(true),
            )
            .build(r"\A(?:\b\w+\b)\z")
            .unwrap();
        let mut context = Context::standard();
        context.types.insert(
            "test::Broken".into(),
            Class::Nominal(Pattern(Arc::new(dfa))),
        );
        let native = Kind::Nominal(
            NominalScalar::new("test::Broken", context.catalog(), context.revision(), "é").unwrap(),
        );
        for boxing in [Boxing::Auto, Boxing::None, Boxing::All] {
            assert!(matches!(
                jeto::encode(&native, &context, None, boxing, Limits::default()),
                Err(Error::Invalid {
                    reason: "nominal matcher failed",
                    ..
                })
            ));
        }
        assert!(matches!(
            jeto::decode(
                "\"é\"".as_bytes(),
                &context,
                Some("test::Broken"),
                Limits::default()
            ),
            Err(Error::Invalid {
                reason: "nominal matcher failed",
                ..
            })
        ));
    }
}
