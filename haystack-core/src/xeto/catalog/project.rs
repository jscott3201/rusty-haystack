//! Explicit project-owned Dict declarations layered over a pinned selection.
use super::*;

impl Catalog {
    /// Add or replace one project-owned Dict library. Its bytes are identified by
    /// their own digest and never represented as upstream declarations.
    pub fn with_project(
        &self,
        library: &str,
        source: &str,
        markers: &[(&str, &str)],
    ) -> Result<Self, ProfileError> {
        if library.is_empty()
            || library.len() > 128
            || !library
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'.')
            || library.starts_with("sys")
            || library.starts_with("ph")
            || source.len() > 65_536
            || markers.len() > 128
        {
            return Err(source_error(
                "project",
                "invalid or oversized project selection",
            ));
        }
        let identity = ProfileSource {
            path: format!("project/{library}/lib.xeto"),
            sha256: Sha256::digest(source.as_bytes())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
            role: "project".into(),
            library: library.into(),
            lines: vec![[1, source.lines().count().max(1)]],
            declarations: Vec::new(),
        };
        let parsed = ExtractedSource {
            identity: identity.clone(),
            text: source.into(),
            original_lines: (1..=source.lines().count()).collect(),
        }
        .parse()?;
        if parsed.specs.is_empty() || parsed.specs.len() > 128 {
            return Err(resolve_error(
                &identity,
                library,
                "empty or oversized declaration selection",
            ));
        }
        let pragma = parsed.pragma.ok_or_else(|| {
            resolve_error(&identity, "pragma", "project library requires metadata")
        })?;
        if !pragma.name.is_empty()
            || pragma.version.is_empty()
            || !pragma
                .version
                .split('.')
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(resolve_error(
                &identity,
                "pragma",
                "invalid project library identity",
            ));
        }
        let mut result = self.clone();
        if result
            .pragmas
            .get(library)
            .is_some_and(|(_, source)| source.role != "project")
        {
            return Err(resolve_error(
                &identity,
                library,
                "cannot replace pinned library",
            ));
        }
        result.specs.retain(|_, entry| entry.spec.lib != library);
        result
            .marker_bindings
            .retain(|_, qname| !qname.starts_with(&format!("{library}::")));
        result
            .pragmas
            .insert(library.into(), (pragma.clone(), identity.clone()));
        for dependency in &pragma.depends {
            let Some((loaded, _)) = result.pragmas.get(dependency) else {
                return Err(resolve_error(
                    &identity,
                    library,
                    "missing project dependency",
                ));
            };
            if dependency == library
                || dependency_version(&pragma, dependency) != Some(loaded.version.as_str())
            {
                return Err(resolve_error(
                    &identity,
                    library,
                    "invalid project dependency version",
                ));
            }
        }
        let mut selected = Vec::new();
        for definition in parsed.specs {
            if definition.is_augmentation
                || definition.slots.len() > 256
                || definition.default.is_some()
            {
                return Err(resolve_error(
                    &identity,
                    &definition.name,
                    "unsupported project declaration",
                ));
            }
            validate_slot_syntax(&definition.slots, &identity, &definition.name)?;
            if definition
                .slots
                .iter()
                .any(|slot| !slot.children.is_empty())
            {
                return Err(resolve_error(
                    &identity,
                    &definition.name,
                    "inline structural refinements are not selected",
                ));
            }
            let spec = spec_from_def(&definition, library);
            selected.push(spec.qname.clone());
            insert_spec(
                &mut result.specs,
                AdmittedSpec {
                    spec,
                    source: identity.clone(),
                    member_of: None,
                },
            )?;
        }
        let mut names = BTreeSet::new();
        for entry in result.specs.values() {
            names.insert(entry.spec.qname.clone());
            collect_slot_names(
                &entry.spec.qname,
                &entry.spec.slots,
                &mut names,
                &entry.source,
            )?;
        }
        for qname in &selected {
            let entry = result.specs.get_mut(qname).expect("selected declaration");
            let base = entry.spec.base.as_deref().ok_or_else(|| {
                resolve_error(
                    &identity,
                    qname,
                    "project declaration requires explicit base",
                )
            })?;
            entry.spec.base = Some(resolve_name(
                base,
                library,
                &result.pragmas,
                &names,
                &identity,
                qname,
            )?);
            resolve_slots(
                &mut entry.spec.slots,
                library,
                qname,
                &result.pragmas,
                &names,
                &identity,
            )?;
            resolve_slot_of(
                &mut entry.spec.slots,
                library,
                &result.pragmas,
                &names,
                &identity,
                qname,
            )?;
            validate_meta(&entry.spec.meta, &result.metadata, &identity, qname)?;
        }
        validate_cycles(&result.specs)?;
        let types = result.specs.clone();
        for qname in &selected {
            if !result.derives_from(qname, "sys::Dict") {
                return Err(resolve_error(
                    &identity,
                    qname,
                    "project selection admits Dict types only",
                ));
            }
            let entry = result.specs.get_mut(qname).expect("selected declaration");
            decode_slots(
                &mut entry.spec.slots,
                &types,
                &result.enums,
                &result.metadata,
                &identity,
                qname,
            )?;
        }
        for &(marker, qname) in markers {
            let Some(entry) = result.specs.get(qname) else {
                return Err(resolve_error(
                    &identity,
                    qname,
                    "unknown marker association",
                ));
            };
            if entry.spec.lib != library
                || marker.is_empty()
                || marker.len() > 128
                || !marker
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                || result
                    .marker_bindings
                    .insert(marker.into(), qname.into())
                    .is_some()
            {
                return Err(resolve_error(
                    &identity,
                    qname,
                    "invalid or ambiguous marker association",
                ));
            }
        }
        result.libraries.insert(
            library.into(),
            AdmittedLibrary {
                name: library.into(),
                doc: pragma.doc,
                version: pragma.version,
                maturity: match pragma.meta.get("maturity") {
                    Some(Kind::Str(value)) => value.clone(),
                    _ => "project".into(),
                },
                depends: pragma.depends,
                declarations: selected.clone(),
                metadata_fields: Vec::new(),
                complete: false,
            },
        );
        result
            .provenance
            .files
            .retain(|source| !(source.role == "project" && source.library == library));
        let mut identity = identity;
        identity.declarations = selected;
        result.provenance.files.push(identity);
        result.compiled = result.compile()?;
        // Mirror the codec bounds activation compiles against (at most 256
        // context definitions, 1024-byte patterns, bounded source bytes): a
        // selection admitted here never fails later with an opaque error.
        for operation in result.operations() {
            super::callable::CallableContext::new(&result, operation)?;
        }
        Ok(result)
    }

    pub(crate) fn derives_from(&self, actual: &str, expected: &str) -> bool {
        let mut next = Some(actual);
        for _ in 0..=self.specs.len() {
            let Some(name) = next else {
                return false;
            };
            if name == expected {
                return true;
            }
            next = self
                .specs
                .get(name)
                .and_then(|entry| entry.spec.base.as_deref());
        }
        false
    }

    /// Selection identity is independent of runtime publication generation and
    /// native nominal provenance. Equal source bytes do not grant currentness.
    pub fn selection_identity(&self) -> String {
        let mut digest = Sha256::new();
        for (qname, entry) in &self.specs {
            digest.update(qname.as_bytes());
            digest.update([0]);
            digest.update(entry.source.sha256.as_bytes());
            digest.update([0]);
        }
        for (marker, qname) in &self.marker_bindings {
            digest.update(marker.as_bytes());
            digest.update([0]);
            digest.update(qname.as_bytes());
            digest.update([0]);
        }
        digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}
