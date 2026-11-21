use super::*;
use haystack_core::ontology::DefNamespace;
use serde::Deserialize;

#[derive(Clone, Copy, Deserialize)]
pub(in crate::commands::mcp) enum SemanticProfile {
    #[serde(rename = "haystack-h4-semantics-v1")]
    H4,
}
#[derive(Clone)]
pub(in crate::commands::mcp) struct SemanticScope {
    libraries: BTreeSet<String>,
    definitions: BTreeSet<String>,
}
impl SemanticScope {
    pub(super) fn allows(&self, kind: CatalogKind, name: &str) -> bool {
        match kind {
            CatalogKind::Library => self.libraries.contains(name),
            CatalogKind::Definition => self.definitions.contains(name),
            _ => false,
        }
    }
}
pub(in crate::commands::mcp) fn semantic_namespace(
    selected: Option<SemanticProfile>,
) -> Result<Option<Arc<DefNamespace>>, String> {
    selected
        .map(|_| {
            DefNamespace::load_bundled_h4()
                .map(Arc::new)
                .map_err(|_| "bundled H4 semantic namespace is unavailable".into())
        })
        .transpose()
}
pub(in crate::commands::mcp) fn semantic_scope(
    namespace: Option<&Arc<DefNamespace>>,
    libraries: &[String],
    definitions: &[String],
) -> Result<Option<Arc<SemanticScope>>, String> {
    let Some(namespace) = namespace else {
        return if libraries.is_empty() && definitions.is_empty() {
            Ok(None)
        } else {
            Err("h4_libraries and h4_definitions require semantic_resources".into())
        };
    };
    if libraries.len() > 32
        || definitions.len() > 256
        || libraries
            .iter()
            .chain(definitions)
            .any(|name| name.is_empty() || name.len() > 256)
    {
        return Err("H4 semantic grants exceed their limits".into());
    }
    let libs: BTreeSet<_> = libraries.iter().cloned().collect();
    let defs: BTreeSet<_> = definitions.iter().cloned().collect();
    if libs.len() != libraries.len()
        || defs.len() != definitions.len()
        || libs.iter().any(|name| !namespace.libs().contains_key(name))
        || defs.iter().any(|name| {
            namespace
                .get_def(name)
                .is_none_or(|def| !libs.contains(&def.lib))
        })
    {
        return Err(
            "H4 grants must name unique bundled definitions and their explicitly granted libraries"
                .into(),
        );
    }
    Ok(Some(Arc::new(SemanticScope {
        libraries: libs,
        definitions: defs,
    })))
}
