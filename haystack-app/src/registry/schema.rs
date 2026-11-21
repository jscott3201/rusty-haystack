//! Caller-scoped document assembly; routing and schema share Entry::visible.
use super::*;
use haystack_core::xeto::catalog::schema::{SchemaControl, SchemaError, SchemaProjector};
use serde_json::{Value, json};
use std::collections::BTreeMap;
struct Control<'a> {
    policy: &'a dyn PolicySnapshot,
    budget: &'a mut Budget,
}
impl SchemaControl for Control<'_> {
    type Error = ReadError;
    fn charge(&mut self, work: usize, retained: usize) -> Result<(), ReadError> {
        self.budget.charge(BudgetKind::Work, work)?;
        self.budget.charge(BudgetKind::Retained, retained)
    }
    fn depth(&mut self, depth: usize) -> Result<(), ReadError> {
        self.budget.depth(depth)
    }
    fn visible(&self, qname: &str) -> bool {
        self.policy.catalog(CatalogKind::Spec, qname)
    }
}
impl Registry {
    pub(crate) fn schema(
        &self,
        policy: &dyn PolicySnapshot,
        selection: Selection,
        budget: &mut Budget,
    ) -> Result<Value, ApiError> {
        budget.charge(BudgetKind::Retained, 64 * 1024)?;
        budget.charge(
            BudgetKind::Work,
            self.entries.len() + self.catalog().declarations().count(),
        )?;
        let mut projector = SchemaProjector::new(self.catalog(), Control { policy, budget });
        let mut paths = BTreeMap::new();
        let mut diagnostics = Vec::new();
        let mut supported = Vec::new();
        let mut extra = BTreeMap::new();
        for entry in &self.entries {
            if !entry.visible(policy, selection) {
                continue;
            }
            let name = &entry.identity.qname;
            match projector.callable(name) {
                Ok((mut arguments, result)) => {
                    arguments["additionalProperties"] = json!(!entry.wire.strict_arguments);
                    if !entry.wire.strict_arguments {
                        arguments["description"] = json!(
                            "This compatibility function ignores undeclared arguments after contextual decoding; only declared parameters are bound."
                        );
                    }
                    let argument_name = format!("{name}Arguments");
                    extra.insert(argument_name.clone(), arguments);
                    supported.push(name.clone());
                    paths.insert(format!("/api/{name}"),json!({"post":{
                        "operationId": name.replace("::","_").replace('.',"_"),
                        "tags":["typed"],
                        "description":"Qualified POST, Xeto-Version: 5, Accept: application/json;box=none. Exact unboxing may return 406 for dynamically typed values. JSON schema cannot validate nominal identity, graph references, authorization or execution outcomes.",
                        "parameters":[{"name":"Xeto-Version","in":"header","required":true,"schema":{"type":"string","enum":["5"]}},{"name":"Accept","in":"header","required":true,"schema":{"type":"string","enum":["application/json;box=none"]}}],
                        "requestBody":{"required":true,"content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{argument_name}")}}}},
                        "responses":{"200":{"description":"Contextual unboxed result","content":{"application/json":{"schema":result}}}}
                    }}));
                }
                Err(SchemaError::Unsupported(reason)) => {
                    diagnostics.push(json!({"declaration":name,"reason":reason,"kind":"function"}))
                }
                Err(SchemaError::Control(error)) => return Err(error.into()),
            }
        }
        // Concrete admitted records are additional schema entry points. Never
        // narrow generic function returns to the contents of a fixture/graph.
        for declaration in self.catalog().declarations() {
            let spec = &declaration.spec;
            if declaration.member_of.is_some() || !policy.catalog(CatalogKind::Spec, &spec.qname) {
                continue;
            }
            match projector.definition(&spec.qname) {
                Ok(_) => (),
                Err(SchemaError::Unsupported(reason)) => diagnostics
                    .push(json!({"declaration":spec.qname,"reason":reason,"kind":"type"})),
                Err(SchemaError::Control(error)) => return Err(error.into()),
            }
        }
        let mut definitions = projector.definitions();
        definitions.extend(extra);
        // This is the actual terminal wire envelope inventory, not invented
        // statuses from upstream documentation. Subtype schemas are admitted.
        let variants = [
            ApiError::InvalidArgs,
            ApiError::UnsupportedVersion,
            ApiError::UnsupportedMediaType,
            ApiError::NotAcceptable,
            ApiError::NotImplemented,
            ApiError::InvalidPath,
            ApiError::UnknownFunction(String::new()),
            ApiError::AmbiguousFunction {
                name: String::new(),
                candidates: vec![],
            },
            ApiError::MethodNotAllowed,
            ApiError::UnknownEntity,
            ApiError::Permission,
            ApiError::AuthRequired,
            ApiError::AuthRejected,
            ApiError::AuthMalformed,
            ApiError::Timeout,
            ApiError::Unavailable,
            ApiError::Internal,
        ];
        let mut errors: BTreeMap<u16, Vec<Value>> = BTreeMap::new();
        for variant in variants {
            budget.charge(BudgetKind::Work, 1)?;
            budget.charge(BudgetKind::Retained, 8192)?;
            let value: Value =
                serde_json::from_str(&variant.json()).map_err(|_| ApiError::Internal)?;
            let name = value["spec"].as_str().ok_or(ApiError::Internal)?;
            if !definitions.contains_key(name) {
                continue;
            }
            let key = format!("{name}{}Response", variant.status());
            let mut schema = definitions[name].clone();
            schema["properties"]["spec"] = json!({"type":"string","enum":[name]});
            schema["properties"]["status"] = json!({"type":"integer","enum":[variant.status()]});
            let required = schema["required"]
                .as_array_mut()
                .ok_or(ApiError::Internal)?;
            if !required.iter().any(|field| field == "spec") {
                required.push(json!("spec"));
            }
            definitions.insert(key.clone(), schema);
            let reference = json!({"$ref":format!("#/components/schemas/{key}")});
            let choices = errors.entry(variant.status()).or_default();
            if !choices.contains(&reference) {
                choices.push(reference);
            }
        }
        for path in paths.values_mut() {
            for (status, choices) in &errors {
                budget.charge(BudgetKind::Retained, choices.len() * 256 + 512)?;
                let schema = if choices.len() == 1 {
                    choices[0].clone()
                } else {
                    json!({"anyOf":choices})
                };
                path["post"]["responses"][status.to_string()] = json!({"description":"Typed ApiErr; HTTP status remains authoritative","content":{"application/json":{"schema":schema}}});
            }
        }
        supported.sort();
        diagnostics.sort_by(|a, b| {
            a["declaration"]
                .as_str()
                .cmp(&b["declaration"].as_str())
                .then(a["kind"].as_str().cmp(&b["kind"].as_str()))
        });
        let definitions: BTreeMap<_, _> = definitions
            .into_iter()
            .map(|(name, mut schema)| {
                rewrite_refs(&mut schema);
                (component_key(&name), schema)
            })
            .collect();
        for path in paths.values_mut() {
            rewrite_refs(path);
        }
        let mut retained = 0usize;
        for (name, schema) in &definitions {
            retained = retained
                .saturating_add(name.len() + 128)
                .saturating_add(value_cost(schema, budget)?);
        }
        // Reserve all definition copies before building the standalone library
        // and document. A larger output ceiling must not itself cost memory.
        budget.charge(BudgetKind::Retained, retained.saturating_mul(4))?;
        let mut standalone =
            json!({"$schema":"https://json-schema.org/draft/2020-12/schema", "$defs":definitions});
        standalone_refs(&mut standalone);
        budget.charge(BudgetKind::Retained, 64 * 1024)?;
        Ok(
            json!({"openapi":"3.1.0","jsonSchemaDialect":"https://json-schema.org/draft/2020-12/schema","info":{"title":"Rusty Haystack admitted typed API","version":"1.0.0","description":"Project-owned schema export for the selected typed v5 box:none profile. This is not upstream sys.api::openapi or a complete Haystack 5 implementation."},"x-rusty-haystack-json-schema":standalone,"paths":paths,"components":{"schemas":definitions},"x-rusty-haystack-profile":{"operations":supported,"excluded":diagnostics,"qualification":"JSON shape validation does not establish nominal identity, reference validity, union branch identity, exact unboxing eligibility, or device effect. Optional Dict members are absent, not null. Parameter null is normalized to absence."}}),
        )
    }
}

fn rewrite_refs(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                if key == "$ref" {
                    if let Some(reference) = value.as_str()
                        && let Some(name) = reference.strip_prefix("#/components/schemas/")
                    {
                        *value = json!(format!("#/components/schemas/{}", component_key(name)));
                    }
                } else {
                    rewrite_refs(value);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                rewrite_refs(value);
            }
        }
        _ => (),
    }
}

fn standalone_refs(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                if key == "$ref" {
                    if let Some(reference) = value.as_str() {
                        *value = json!(reference.replace("#/components/schemas/", "#/$defs/"));
                    }
                } else {
                    standalone_refs(value);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                standalone_refs(value);
            }
        }
        _ => (),
    }
}

// Prefix-free escaping: underscore itself is escaped, so a source `_c` can
// never collide with an escaped colon. Dots already satisfy OpenAPI's grammar.
fn component_key(name: &str) -> String {
    name.chars().fold(String::new(), |mut out, ch| {
        match ch {
            '_' => out.push_str("_u"),
            ':' => out.push_str("_c"),
            _ => out.push(ch),
        }
        out
    })
}
fn value_cost(value: &Value, budget: &mut Budget) -> Result<usize, ReadError> {
    budget.charge(BudgetKind::Work, 1)?;
    let mut size = 128usize;
    match value {
        Value::Object(fields) => {
            for (name, value) in fields {
                budget.charge(BudgetKind::Work, name.len())?;
                size = size
                    .saturating_add(name.len() + 128)
                    .saturating_add(value_cost(value, budget)?);
            }
        }
        Value::Array(values) => {
            for value in values {
                size = size.saturating_add(value_cost(value, budget)?);
            }
        }
        Value::String(text) => {
            budget.charge(BudgetKind::Work, text.len())?;
            size = size.saturating_add(text.len());
        }
        _ => (),
    }
    Ok(size)
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_selected_binding_is_exported_or_explicitly_excluded() {
        use crate::{AllowAll, CancellationToken, ReadLimits};
        use std::{
            sync::Arc,
            time::{Duration, Instant},
        };
        let registry = super::Registry::pinned().unwrap();
        let selection = super::Selection {
            history: true,
            history_mutations: true,
            watches: true,
            commands: true,
        };
        let mut budget = super::Budget::new(
            Arc::new(ReadLimits::default()),
            Instant::now() + Duration::from_secs(5),
            CancellationToken::new(),
        );
        let document = registry.schema(&AllowAll, selection, &mut budget).unwrap();
        let mut names: std::collections::BTreeSet<_> =
            document["x-rusty-haystack-profile"]["operations"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
        for excluded in document["x-rusty-haystack-profile"]["excluded"]
            .as_array()
            .unwrap()
        {
            if excluded["kind"] == "function" {
                assert!(names.insert(excluded["declaration"].as_str().unwrap()));
            }
        }
        assert_eq!(
            names,
            super::super::BINDINGS
                .iter()
                .map(|(name, _)| *name)
                .collect()
        );
    }
    #[test]
    fn component_names_do_not_collapse_source_separators() {
        let names = ["a::B", "a_c_cB", "a__b::C", "a::b__C", "a.b::C", "a::b.C"];
        let keys: std::collections::BTreeSet<_> = names
            .iter()
            .map(|name| super::component_key(name))
            .collect();
        assert_eq!(keys.len(), names.len());
        assert!(keys.iter().all(|key| {
            key.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        }));
    }
}
