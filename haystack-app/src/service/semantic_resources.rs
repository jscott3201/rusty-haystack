//! Fixed H4 semantic projections. Identifiers are local authenticated names,
//! never paths or URLs to resolve. Possession does not grant catalog authority.
use super::*;
use serde_json::json;

pub const H4_RESOURCE_PROFILE: &str = "haystack-h4-semantics-v1";
pub const H4_RESOURCE_URI_BYTES: usize = 1024;
const MAX_PAGE: usize = 32;
const URI_DOMAIN: &[u8] = b"h4-resource-v1";
const PAGE_DOMAIN: &[u8] = b"h4-resource-page-v1";

#[derive(Debug)]
pub struct H4Resource {
    pub uri: String,
    pub name: String,
    pub kind: &'static str,
    pub library: String,
}
#[derive(Debug)]
pub struct H4ResourcePage {
    pub resources: Vec<H4Resource>,
    /// No cursor means the complete authorized inventory was enumerated.
    pub cursor: Option<String>,
}
#[derive(Debug)]
pub struct H4ResourceContent {
    pub uri: String,
    /// One versioned JSON envelope containing the shared H4 JSON grid.
    pub text: String,
}
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum KindKey {
    Library,
    Definition,
}
impl KindKey {
    fn code(self) -> &'static str {
        match self {
            Self::Library => "lib",
            Self::Definition => "def",
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Library => "library",
            Self::Definition => "definition",
        }
    }
}
struct Name {
    kind: KindKey,
    library: String,
    name: String,
}
#[derive(Clone, Copy)]
struct NameRef<'a> {
    kind: KindKey,
    library: &'a str,
    name: &'a str,
}
struct Observation {
    dataset: String,
    incarnation: String,
    catalog: u64,
}
impl Observation {
    fn new(inner: &Inner, graph: &haystack_core::graph::EntityGraph) -> Self {
        Self {
            dataset: hex(&inner.dataset),
            incarnation: hex(&graph.incarnation()),
            catalog: graph.catalog_generation(),
        }
    }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-' || c == b':')
}
fn eligible(lib: &haystack_core::ontology::Lib) -> bool {
    // Compatibility Xeto-only libraries have no H4 definition inventory.
    // This profile intentionally covers nonempty H4 definition libraries.
    !lib.defs.is_empty()
}
fn visible(kind: KindKey, name: &str, library: &str, policy: &dyn PolicySnapshot) -> bool {
    policy.catalog(CatalogKind::Library, library)
        && policy.semantic_resource(CatalogKind::Library, library)
        && match kind {
            KindKey::Library => policy.operation(ReadOperation::Libraries),
            KindKey::Definition => {
                policy.operation(ReadOperation::Definitions)
                    && policy.catalog(CatalogKind::Definition, name)
                    && policy.semantic_resource(CatalogKind::Definition, name)
            }
        }
}

impl ReadAdmission {
    /// Enumerate both H4 kinds in (library, definition), then lexical-name order.
    /// One cumulative budget and graph observation cover the entire scan/page.
    /// Names and opaque continuation are invalidated by a service restart,
    /// graph incarnation, catalog publication or principal/scope change. Entity
    /// writes alone do not invalidate this catalog-only profile. No URI cache
    /// or continuation table is retained; continuations last for that observation.
    pub async fn list_h4_resources_joined(
        self,
        page_size: usize,
        cursor: Option<String>,
        max_output_bytes: usize,
    ) -> Result<H4ResourcePage, ReadError> {
        self.list_h4_resources_with_joined(page_size, cursor, max_output_bytes, Ok)
            .await
    }
    /// Finish bounded adapter encoding inside the original worker/admission.
    /// The shared conservative reservation includes descriptor escaping and
    /// copies. The trusted callback must enforce `max_output_bytes`, remain
    /// finite, and perform no additional lookup or external operation.
    pub async fn list_h4_resources_with_joined<T: Send + 'static>(
        mut self,
        page_size: usize,
        cursor: Option<String>,
        max_output_bytes: usize,
        encode: impl FnOnce(H4ResourcePage) -> Result<T, ReadError> + Send + 'static,
    ) -> Result<T, ReadError> {
        let completion = self.retain_work()?.completion.0.clone();
        let inner = self.inner.clone();
        let result = self
            .run_task(move |principal, budget| {
                let page = inner.list_h4_resources(
                    principal,
                    page_size,
                    cursor,
                    max_output_bytes,
                    budget,
                )?;
                budget.check()?;
                let encoded = encode(page)?;
                budget.check()?;
                Ok(encoded)
            })
            .await;
        completion.wait().await;
        result
    }
    /// Exact read with current policy revalidation. Missing, denied, malformed,
    /// foreign and stale identities all return `Unavailable`; rediscover names.
    pub async fn read_h4_resource_joined(
        self,
        uri: String,
        max_output_bytes: usize,
    ) -> Result<H4ResourceContent, ReadError> {
        self.read_h4_resource_with_joined(uri, max_output_bytes, Ok)
            .await
    }
    /// As [`Self::list_h4_resources_with_joined`], retaining the same worker
    /// through final content encoding. Shared reservations include JSON text
    /// escaping; the trusted callback must enforce the passed response cap.
    pub async fn read_h4_resource_with_joined<T: Send + 'static>(
        mut self,
        uri: String,
        max_output_bytes: usize,
        encode: impl FnOnce(H4ResourceContent) -> Result<T, ReadError> + Send + 'static,
    ) -> Result<T, ReadError> {
        let completion = self.retain_work()?.completion.0.clone();
        let inner = self.inner.clone();
        let result = self
            .run_task(move |principal, budget| {
                let content = inner.read_h4_resource(principal, uri, max_output_bytes, budget)?;
                budget.check()?;
                let encoded = encode(content)?;
                budget.check()?;
                Ok(encoded)
            })
            .await;
        completion.wait().await;
        result
    }
}

impl Inner {
    fn resource_policy(
        &self,
        principal: &Principal,
        budget: &mut Budget,
    ) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
        if principal.bytes() > budget.limits.max_input_bytes
            || matches!(principal, Principal::Authenticated { permissions, .. }
                if permissions.len() > budget.limits.max_ids)
        {
            return Err(ReadError::Budget(BudgetKind::Input));
        }
        budget.charge(BudgetKind::Work, principal.bytes())?;
        let policy = self
            .policy
            .snapshot(principal)
            .map_err(|error| match error {
                ReadError::Forbidden => ReadError::Unavailable,
                other => other,
            })?;
        if policy.scope_key().len() > budget.limits.max_input_bytes {
            return Err(ReadError::Budget(BudgetKind::Input));
        }
        budget.check()?;
        Ok(policy)
    }
    fn resource_mac(
        &self,
        unsigned: &str,
        domain: &[u8],
        page_size: usize,
        principal: &Principal,
        policy: &dyn PolicySnapshot,
        budget: &mut Budget,
    ) -> Result<Hmac<Sha256>, ReadError> {
        budget.charge(
            BudgetKind::Work,
            unsigned
                .len()
                .saturating_add(principal.bytes())
                .saturating_add(policy.scope_key().len())
                .saturating_add(256),
        )?;
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.cursor_key).map_err(|_| ReadError::Unavailable)?;
        fn field(mac: &mut Hmac<Sha256>, value: &[u8]) {
            mac.update(&(value.len() as u64).to_be_bytes());
            mac.update(value);
        }
        field(&mut mac, domain);
        field(&mut mac, &(page_size as u64).to_be_bytes());
        field(&mut mac, unsigned.as_bytes());
        field(&mut mac, policy.scope_key().as_bytes());
        match principal {
            Principal::Anonymous => field(&mut mac, b"anonymous"),
            Principal::TrustedEmbedding { subject } => {
                field(&mut mac, b"embedding");
                field(&mut mac, subject.as_bytes());
            }
            Principal::Authenticated {
                subject,
                permissions,
            } => {
                field(&mut mac, b"authenticated");
                field(&mut mac, subject.as_bytes());
                for permission in permissions {
                    field(&mut mac, permission.as_bytes());
                }
            }
        }
        Ok(mac)
    }
    fn resource_token(
        &self,
        observation: &Observation,
        key: NameRef<'_>,
        page_size: usize,
        principal: &Principal,
        policy: &dyn PolicySnapshot,
        budget: &mut Budget,
    ) -> Result<String, ReadError> {
        let NameRef {
            kind,
            library,
            name,
        } = key;
        if !valid_name(library) || !valid_name(name) {
            return Err(ReadError::Unavailable);
        }
        budget.charge(BudgetKind::Retained, H4_RESOURCE_URI_BYTES * 3)?;
        let unsigned = format!(
            "haystack://{}/{}/{}/{}/{}/{}/{}",
            observation.dataset,
            observation.incarnation,
            observation.catalog,
            H4_RESOURCE_PROFILE,
            kind.code(),
            URL_SAFE_NO_PAD.encode(library),
            URL_SAFE_NO_PAD.encode(name)
        );
        let domain = if page_size == 0 {
            URI_DOMAIN
        } else {
            PAGE_DOMAIN
        };
        let mac = self.resource_mac(&unsigned, domain, page_size, principal, policy, budget)?;
        let token = format!(
            "{unsigned}/{}",
            URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
        );
        if token.len() > H4_RESOURCE_URI_BYTES {
            return Err(ReadError::Unavailable);
        }
        Ok(token)
    }
    fn parse_resource(
        &self,
        token: &str,
        observation: &Observation,
        page_size: usize,
        principal: &Principal,
        policy: &dyn PolicySnapshot,
        budget: &mut Budget,
    ) -> Result<Name, ReadError> {
        if token.len() > H4_RESOURCE_URI_BYTES {
            return Err(ReadError::Unavailable);
        }
        budget.charge(BudgetKind::Input, token.len())?;
        budget.charge(BudgetKind::Work, token.len())?;
        budget.charge(BudgetKind::Retained, 4096)?;
        let mut parts = token
            .strip_prefix("haystack://")
            .ok_or(ReadError::Unavailable)?
            .split('/');
        let pieces: [&str; 8] = std::array::from_fn(|_| parts.next().unwrap_or(""));
        if parts.next().is_some()
            || pieces.iter().any(|part| part.is_empty())
            || pieces[0] != observation.dataset
            || pieces[1] != observation.incarnation
            || pieces[2] != observation.catalog.to_string()
            || pieces[3] != H4_RESOURCE_PROFILE
        {
            return Err(ReadError::Unavailable);
        }
        let kind = match pieces[4] {
            "lib" => KindKey::Library,
            "def" => KindKey::Definition,
            _ => return Err(ReadError::Unavailable),
        };
        let decode = |text: &str| -> Result<String, ReadError> {
            let bytes = URL_SAFE_NO_PAD
                .decode(text)
                .map_err(|_| ReadError::Unavailable)?;
            if URL_SAFE_NO_PAD.encode(&bytes) != text {
                return Err(ReadError::Unavailable);
            }
            let name = String::from_utf8(bytes).map_err(|_| ReadError::Unavailable)?;
            if !valid_name(&name) {
                return Err(ReadError::Unavailable);
            }
            Ok(name)
        };
        let library = decode(pieces[5])?;
        let name = decode(pieces[6])?;
        if kind == KindKey::Library && name != library {
            return Err(ReadError::Unavailable);
        }
        let signature = URL_SAFE_NO_PAD
            .decode(pieces[7])
            .map_err(|_| ReadError::Unavailable)?;
        if signature.len() != 32 || URL_SAFE_NO_PAD.encode(&signature) != pieces[7] {
            return Err(ReadError::Unavailable);
        }
        let unsigned = token.rsplit_once('/').ok_or(ReadError::Unavailable)?.0;
        let domain = if page_size == 0 {
            URI_DOMAIN
        } else {
            PAGE_DOMAIN
        };
        self.resource_mac(unsigned, domain, page_size, principal, policy, budget)?
            .verify_slice(&signature)
            .map_err(|_| ReadError::Unavailable)?;
        Ok(Name {
            kind,
            library,
            name,
        })
    }
    fn list_h4_resources(
        &self,
        principal: Principal,
        page_size: usize,
        cursor: Option<String>,
        max_output_bytes: usize,
        budget: &mut Budget,
    ) -> Result<H4ResourcePage, ReadError> {
        if page_size == 0 || page_size > MAX_PAGE || page_size > budget.limits.max_rows {
            return Err(ReadError::InvalidQuery("invalid resource page size"));
        }
        let policy = self.resource_policy(&principal, budget)?;
        if !policy.operation(ReadOperation::Definitions)
            && !policy.operation(ReadOperation::Libraries)
        {
            return Err(ReadError::Unavailable);
        }
        loop {
            if let Some(result) = self.graph.read_for(budget.wait_quantum()?, |graph| {
                budget.charge(BudgetKind::Retained, 256)?;
                let observation = Observation::new(self, graph);
                let after = cursor
                    .as_deref()
                    .map(|token| {
                        self.parse_resource(
                            token,
                            &observation,
                            page_size,
                            &principal,
                            policy.as_ref(),
                            budget,
                        )
                    })
                    .transpose()?;
                let ns = graph.namespace().ok_or(ReadError::Unavailable)?;
                let mut selected = BTreeMap::new();
                for (name, lib) in ns.libs() {
                    select_resource(
                        &mut selected,
                        NameRef {
                            kind: KindKey::Library,
                            library: name,
                            name,
                        },
                        eligible(lib) && visible(KindKey::Library, name, name, policy.as_ref()),
                        after.as_ref(),
                        page_size,
                        budget,
                    )?;
                }
                for (name, def) in ns.defs() {
                    let allowed = ns
                        .libs()
                        .get(&def.lib)
                        .is_some_and(|lib| eligible(lib) && lib.defs.contains_key(name))
                        && visible(KindKey::Definition, name, &def.lib, policy.as_ref());
                    select_resource(
                        &mut selected,
                        NameRef {
                            kind: KindKey::Definition,
                            library: &def.lib,
                            name,
                        },
                        allowed,
                        after.as_ref(),
                        page_size,
                        budget,
                    )?;
                }
                let more = selected.len() > page_size;
                if more {
                    selected.pop_last();
                }
                let mut resources = Vec::new();
                let mut next = None;
                for ((kind, name), library) in selected {
                    let key = NameRef {
                        kind,
                        library,
                        name,
                    };
                    let uri = self.resource_token(
                        &observation,
                        key,
                        0,
                        &principal,
                        policy.as_ref(),
                        budget,
                    )?;
                    if more {
                        next = Some(self.resource_token(
                            &observation,
                            key,
                            page_size,
                            &principal,
                            policy.as_ref(),
                            budget,
                        )?);
                    }
                    resources.push(H4Resource {
                        uri,
                        name: budget.copy_string(name)?,
                        kind: kind.name(),
                        library: budget.copy_string(library)?,
                    });
                }
                // Reserve conservative JSON-RPC descriptor/escaping copies before
                // handing bounded native strings to an adapter.
                let size = resources
                    .iter()
                    .fold(1024usize, |n, item| {
                        n.saturating_add(
                            item.uri
                                .len()
                                .saturating_add(item.name.len())
                                .saturating_add(item.library.len())
                                .saturating_mul(6)
                                .saturating_add(512),
                        )
                    })
                    .saturating_add(
                        next.as_ref()
                            .map_or(0, |token| token.len().saturating_mul(6)),
                    );
                reserve_output(size, max_output_bytes, budget)?;
                Ok(H4ResourcePage {
                    resources,
                    cursor: next,
                })
            }) {
                return result;
            }
        }
    }
    fn read_h4_resource(
        &self,
        principal: Principal,
        uri: String,
        max_output_bytes: usize,
        budget: &mut Budget,
    ) -> Result<H4ResourceContent, ReadError> {
        let policy = self.resource_policy(&principal, budget)?;
        loop {
            if let Some(result) = self.graph.read_for(budget.wait_quantum()?, |graph| {
                budget.charge(BudgetKind::Retained, 256)?;
                let observation = Observation::new(self, graph);
                let name = self.parse_resource(&uri, &observation, 0, &principal, policy.as_ref(), budget)?;
                let ns = graph.namespace().ok_or(ReadError::Unavailable)?;
                let lib = ns.libs().get(&name.library).filter(|lib| eligible(lib))
                    .ok_or(ReadError::Unavailable)?;
                if !visible(name.kind, &name.name, &name.library, policy.as_ref()) {
                    return Err(ReadError::Unavailable);
                }
                catalog_visit(&name.name, budget)?;
                let item = match name.kind {
                    KindKey::Library => CatalogRow::Library(lib),
                    KindKey::Definition => CatalogRow::Definition(ns.get_def(&name.name)
                        .filter(|def| def.lib == name.library && lib.defs.contains_key(&name.name))
                        .ok_or(ReadError::Unavailable)?),
                };
                let row = catalog_row(item, ns, &mut View { graph, policy: policy.as_ref(), budget })?;
                let grid = output::grid(vec![row], true, None, budget)?;
                let ReadOutput::H4 { body, .. } = output::encode(grid, OutputProfile::H4(H4Codec::Json), budget)?
                    else { return Err(ReadError::Projection); };
                budget.charge(BudgetKind::Retained, 16 * 1024)?;
                let metadata = json!({"profile":H4_RESOURCE_PROFILE,"uri":uri,
                    "kind":name.kind.name(),"name":name.name,"library":name.library,
                    "observation":{"dataset":observation.dataset,"incarnation":observation.incarnation,
                        "catalog":observation.catalog},
                    "provenance":{"source":"attached-h4-namespace","projection":"def-lib-doc-or-library-version-v1"}});
                // Fixed bounded metadata plus already-budgeted shared H4 bytes.
                let bound = body.len().saturating_add(H4_RESOURCE_URI_BYTES * 6).saturating_add(4096);
                reserve_output(bound.saturating_mul(6).saturating_add(2048), max_output_bytes, budget)?;
                let mut text = serde_json::to_string(&metadata).map_err(|_| ReadError::Projection)?;
                text.pop();
                text.push_str(",\"grid\":");
                text.push_str(std::str::from_utf8(&body).map_err(|_| ReadError::Projection)?);
                text.push('}');
                budget.check()?;
                Ok(H4ResourceContent { uri: uri.clone(), text })
            }) { return result; }
        }
    }
}
fn reserve_output(bytes: usize, cap: usize, budget: &mut Budget) -> Result<(), ReadError> {
    if bytes > cap.min(budget.limits.max_output_bytes) {
        return Err(ReadError::Budget(BudgetKind::Output));
    }
    budget.charge(BudgetKind::Retained, bytes.saturating_mul(2))?;
    budget.charge(BudgetKind::Work, bytes)?;
    budget.depth(5)
}

fn select_resource<'a>(
    selected: &mut BTreeMap<(KindKey, &'a str), &'a str>,
    key: NameRef<'a>,
    allowed: bool,
    after: Option<&Name>,
    page_size: usize,
    budget: &mut Budget,
) -> Result<(), ReadError> {
    let NameRef {
        kind,
        library,
        name,
    } = key;
    catalog_visit(name, budget)?;
    if !allowed {
        return Ok(());
    }
    if !valid_name(name) || !valid_name(library) {
        return Err(ReadError::Projection);
    }
    if after.is_some_and(|after| (kind, name) <= (after.kind, after.name.as_str())) {
        return Ok(());
    }
    if selected.len() > page_size
        && selected
            .last_key_value()
            .is_some_and(|(last, _)| (kind, name) >= *last)
    {
        return Ok(());
    }
    budget.charge(BudgetKind::Retained, 128)?;
    selected.insert((kind, name), library);
    if selected.len() > page_size + 1 {
        selected.pop_last();
    }
    Ok(())
}
