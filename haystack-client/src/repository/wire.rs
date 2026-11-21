//! Closed DTO decoding for the pinned repository contextual JSON profile.
use super::*;
use haystack_core::{
    codecs::jeto,
    kinds::{HDateTime, Kind},
};
use serde_json::{Map as JsonMap, Value as JsonValue};
use std::collections::BTreeSet;

/// Repository display metadata. Bounded server-defined tags remain available.
#[derive(Clone, Debug, PartialEq)]
pub struct RepoPing {
    pub dis: String,
    pub extra: JsonMap<String, JsonValue>,
}
/// Search results in server relevance order; absence of `total` means unreported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoSearch {
    pub libs: Vec<RepoLibSummary>,
    pub more: bool,
    pub total: Option<u64>,
}
/// Summary of one library. No dependency or installation inference is made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoLibSummary {
    pub lib: String,
    pub latest_version: LibraryVersion,
    pub latest_maturity: Maturity,
    pub latest_published: Option<HDateTime>,
    pub latest_stable: Option<LibraryVersion>,
    pub deprecated: Option<String>,
    pub doc: Option<String>,
}

// Local wire syntax retains numeric tokens until their declared DTO type is
// known. Scalar semantics come from the existing core Jeto decoder, never f64.
// This tree is bounded and private; it is not another public Haystack value model.
type Map<K, V> = std::collections::BTreeMap<K, V>;
enum Value {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Value>),
    Object(Map<String, Value>),
}
impl Value {
    fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }
    fn as_str(&self) -> Option<&str> {
        if let Self::String(s) = self {
            Some(s)
        } else {
            None
        }
    }
    fn as_array(&self) -> Option<&Vec<Self>> {
        if let Self::Array(a) = self {
            Some(a)
        } else {
            None
        }
    }
    fn as_object(&self) -> Option<&Map<String, Self>> {
        if let Self::Object(o) = self {
            Some(o)
        } else {
            None
        }
    }
    fn to_json(&self) -> Result<JsonValue, RepositoryError> {
        Ok(match self {
            Self::Null => JsonValue::Null,
            Self::Bool(value) => JsonValue::Bool(*value),
            // Unknown ping-extension values have no declared Int context. Keep
            // the existing JSON-value representation for that public extension map.
            Self::Number(text) => serde_json::from_str(text)
                .map_err(|_| RepositoryError::Metadata("extension number"))?,
            Self::String(text) => JsonValue::String(text.clone()),
            Self::Array(values) => {
                JsonValue::Array(values.iter().map(Self::to_json).collect::<Result<_, _>>()?)
            }
            Self::Object(values) => JsonValue::Object(
                values
                    .iter()
                    .map(|(key, value)| Ok((key.clone(), value.to_json()?)))
                    .collect::<Result<_, RepositoryError>>()?,
            ),
        })
    }
}
struct JsonParser<'a> {
    text: &'a str,
    at: usize,
    nodes: usize,
    limits: &'a RepositoryLimits,
}
impl JsonParser<'_> {
    fn syntax() -> RepositoryError {
        RepositoryError::Metadata("JSON syntax")
    }
    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.at).copied()
    }
    fn space(&mut self) {
        while self.peek().is_some_and(|b| b" \t\r\n".contains(&b)) {
            self.at += 1;
        }
    }
    fn string(&mut self) -> Result<String, RepositoryError> {
        let start = self.at;
        if self.peek() != Some(b'"') {
            return Err(Self::syntax());
        }
        self.at += 1;
        let mut escaped = false;
        while let Some(byte) = self.peek() {
            self.at += 1;
            if byte == b'"' && !escaped {
                let text: String =
                    serde_json::from_str(&self.text[start..self.at]).map_err(|_| Self::syntax())?;
                if text.len() > self.limits.string_bytes {
                    return Err(RepositoryError::Limit("string bytes"));
                }
                return Ok(text);
            }
            if byte < 0x20 {
                return Err(Self::syntax());
            }
            escaped = byte == b'\\' && !escaped;
        }
        Err(Self::syntax())
    }
    fn number(&mut self) -> Result<Value, RepositoryError> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        match self.peek() {
            Some(b'0') => self.at += 1,
            Some(b'1'..=b'9') => {
                self.at += 1;
                while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                    self.at += 1;
                }
            }
            _ => return Err(Self::syntax()),
        }
        if self.peek() == Some(b'.') {
            self.at += 1;
            let fraction = self.at;
            while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                self.at += 1;
            }
            if self.at == fraction {
                return Err(Self::syntax());
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            let exponent = self.at;
            while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                self.at += 1;
            }
            if self.at == exponent {
                return Err(Self::syntax());
            }
        }
        Ok(Value::Number(self.text[start..self.at].to_owned()))
    }
    fn value(&mut self, depth: usize) -> Result<Value, RepositoryError> {
        if depth > self.limits.metadata_depth {
            return Err(RepositoryError::Limit("JSON depth"));
        }
        if self.nodes >= self.limits.metadata_nodes {
            return Err(RepositoryError::Limit("JSON nodes"));
        }
        self.nodes += 1;
        self.space();
        match self.peek() {
            Some(b'n') if self.text[self.at..].starts_with("null") => {
                self.at += 4;
                Ok(Value::Null)
            }
            Some(b't') if self.text[self.at..].starts_with("true") => {
                self.at += 4;
                Ok(Value::Bool(true))
            }
            Some(b'f') if self.text[self.at..].starts_with("false") => {
                self.at += 5;
                Ok(Value::Bool(false))
            }
            Some(b'"') => self.string().map(Value::String),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(b'[') => {
                self.at += 1;
                self.space();
                let mut values = Vec::new();
                if self.peek() == Some(b']') {
                    self.at += 1;
                    return Ok(Value::Array(values));
                }
                loop {
                    if values.len() >= self.limits.records.max(self.limits.dependencies) {
                        return Err(RepositoryError::Limit("array entries"));
                    }
                    values.push(self.value(depth + 1)?);
                    self.space();
                    match self.peek() {
                        Some(b',') => self.at += 1,
                        Some(b']') => {
                            self.at += 1;
                            return Ok(Value::Array(values));
                        }
                        _ => return Err(Self::syntax()),
                    }
                }
            }
            Some(b'{') => {
                self.at += 1;
                self.space();
                let mut values = Map::new();
                if self.peek() == Some(b'}') {
                    self.at += 1;
                    return Ok(Value::Object(values));
                }
                loop {
                    self.space();
                    let key = self.string()?;
                    if values.contains_key(&key) {
                        return Err(Self::syntax());
                    }
                    if values.len() >= 128 {
                        return Err(RepositoryError::Limit("object members"));
                    }
                    self.space();
                    if self.peek() != Some(b':') {
                        return Err(Self::syntax());
                    }
                    self.at += 1;
                    values.insert(key, self.value(depth + 1)?);
                    self.space();
                    match self.peek() {
                        Some(b',') => self.at += 1,
                        Some(b'}') => {
                            self.at += 1;
                            return Ok(Value::Object(values));
                        }
                        _ => return Err(Self::syntax()),
                    }
                }
            }
            _ => Err(Self::syntax()),
        }
    }
}
fn json(bytes: &[u8], limits: &RepositoryLimits) -> Result<Value, RepositoryError> {
    if bytes.len() > limits.metadata_bytes {
        return Err(RepositoryError::Limit("metadata bytes"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| JsonParser::syntax())?;
    let mut parser = JsonParser {
        text,
        at: 0,
        nodes: 0,
        limits,
    };
    let value = parser.value(0)?;
    parser.space();
    if parser.at != text.len() {
        return Err(JsonParser::syntax());
    }
    Ok(value)
}
fn integer(value: &Value, limits: &RepositoryLimits) -> Result<i64, RepositoryError> {
    let encoded;
    let bytes = match value {
        Value::Number(text) => text.as_bytes(),
        Value::String(text) => {
            encoded = serde_json::to_vec(text).map_err(|_| RepositoryError::Metadata("integer"))?;
            &encoded
        }
        _ => return Err(RepositoryError::Metadata("integer")),
    };
    let limits = jeto::Limits {
        max_input_bytes: limits.metadata_bytes,
        max_retained_bytes: limits.metadata_bytes * 4,
        max_work: limits.metadata_bytes * 8,
        max_nodes: 2,
        max_depth: 1,
        ..Default::default()
    };
    match jeto::decode(bytes, &jeto::Context::standard(), Some("sys::Int"), limits) {
        Ok(Kind::Int(value)) => Ok(value),
        _ => Err(RepositoryError::Metadata("integer")),
    }
}
fn object(v: &Value) -> Result<&Map<String, Value>, RepositoryError> {
    v.as_object()
        .ok_or(RepositoryError::Metadata("object required"))
}
fn optional<'a>(o: &'a Map<String, Value>, k: &str) -> Option<&'a Value> {
    o.get(k).filter(|v| !v.is_null())
}
fn required<'a>(o: &'a Map<String, Value>, k: &str) -> Result<&'a Value, RepositoryError> {
    optional(o, k).ok_or(RepositoryError::Metadata("required field missing"))
}
fn string(v: &Value) -> Result<&str, RepositoryError> {
    v.as_str()
        .ok_or(RepositoryError::Metadata("string required"))
}
fn text(o: &Map<String, Value>, k: &str) -> Result<String, RepositoryError> {
    Ok(string(required(o, k)?)?.to_owned())
}
fn opt_text(o: &Map<String, Value>, k: &str) -> Result<Option<String>, RepositoryError> {
    optional(o, k)
        .map(|v| string(v).map(str::to_owned))
        .transpose()
}
fn version(v: &Value) -> Result<LibraryVersion, RepositoryError> {
    string(v)?
        .parse()
        .map_err(|_| RepositoryError::Metadata("library version"))
}
fn maturity(v: &Value) -> Result<Maturity, RepositoryError> {
    match string(v)? {
        "alpha" => Ok(Maturity::Alpha),
        "beta" => Ok(Maturity::Beta),
        "stable" => Ok(Maturity::Stable),
        _ => Err(RepositoryError::Metadata("maturity")),
    }
}
fn spec(o: &Map<String, Value>, expected: &str) -> Result<(), RepositoryError> {
    if let Some(v) = optional(o, "spec")
        && string(v)? != expected
    {
        return Err(RepositoryError::Metadata("unexpected spec"));
    }
    if o.contains_key("_kind") || o.contains_key("_type") {
        return Err(RepositoryError::Metadata("boxed metadata"));
    }
    Ok(())
}
pub(super) fn ping(bytes: &[u8], l: &RepositoryLimits) -> Result<RepoPing, RepositoryError> {
    let v = json(bytes, l)?;
    let o = object(&v)?;
    spec(o, "sys.repo::RepoPing")?;
    let dis = text(o, "dis")?;
    let extra = o
        .iter()
        .filter(|(key, _)| !matches!(key.as_str(), "dis" | "spec"))
        .map(|(key, value)| Ok((key.clone(), value.to_json()?)))
        .collect::<Result<_, RepositoryError>>()?;
    Ok(RepoPing { dis, extra })
}
pub(super) fn search(
    bytes: &[u8],
    l: &RepositoryLimits,
    limit: Option<usize>,
) -> Result<RepoSearch, RepositoryError> {
    let v = json(bytes, l)?;
    let o = object(&v)?;
    spec(o, "sys.repo::RepoSearch")?;
    let values = required(o, "libs")?
        .as_array()
        .ok_or(RepositoryError::Metadata("search libraries"))?;
    if values.len() > limit.unwrap_or(l.records) {
        return Err(RepositoryError::Limit("records"));
    }
    let more = match optional(o, "more") {
        None => false,
        Some(Value::String(s)) if s == "✓" => true,
        _ => return Err(RepositoryError::Metadata("more marker")),
    };
    let total = optional(o, "total")
        .map(|v| {
            u64::try_from(integer(v, l)?).map_err(|_| RepositoryError::Metadata("total integer"))
        })
        .transpose()?;
    let mut libs = Vec::new();
    let mut seen = BTreeSet::new();
    for v in values {
        let o = object(v)?;
        spec(o, "sys.repo::RepoLibSummary")?;
        let lib = text(o, "lib")?;
        types::validate_lib(&lib).map_err(|_| RepositoryError::Metadata("library name"))?;
        if !seen.insert(lib.clone()) {
            return Err(RepositoryError::Metadata("duplicate library"));
        }
        let latest_published = optional(o, "latestPublished")
            .map(|v| {
                let encoded = serde_json::to_vec(string(v)?)
                    .map_err(|_| RepositoryError::Metadata("publication time"))?;
                match jeto::decode(
                    &encoded,
                    &jeto::Context::standard(),
                    Some("sys::DateTime"),
                    jeto::Limits::default(),
                ) {
                    Ok(Kind::DateTime(dt)) => Ok(dt),
                    _ => Err(RepositoryError::Metadata("publication time")),
                }
            })
            .transpose()?;
        libs.push(RepoLibSummary {
            lib,
            latest_version: version(required(o, "latestVersion")?)?,
            latest_maturity: maturity(required(o, "latestMaturity")?)?,
            latest_published,
            latest_stable: optional(o, "latestStable").map(version).transpose()?,
            deprecated: opt_text(o, "deprecated")?,
            doc: opt_text(o, "doc")?,
        });
    }
    Ok(RepoSearch { libs, more, total })
}
fn record(v: &Value, l: &RepositoryLimits) -> Result<RepoLibVersion, RepositoryError> {
    let o = object(v)?;
    spec(o, "sys.repo::RepoLibVersion")?;
    let pin = LibraryPin::new(&text(o, "lib")?, version(required(o, "version")?)?)
        .map_err(|_| RepositoryError::Metadata("library name"))?;
    let availability = match string(required(o, "availability")?)? {
        "available" => Availability::Available,
        "yanked" => Availability::Yanked,
        "deleted" => Availability::Deleted,
        _ => return Err(RepositoryError::Metadata("availability")),
    };
    let depends = optional(o, "depends")
        .map(|v| {
            let a = v
                .as_array()
                .ok_or(RepositoryError::Metadata("dependencies"))?;
            if a.len() > l.dependencies {
                return Err(RepositoryError::Limit("dependencies"));
            }
            let mut seen = BTreeSet::new();
            let mut deps = Vec::new();
            for v in a {
                let o = object(v)?;
                spec(o, "sys::LibDepend")?;
                let lib = text(o, "lib")?;
                types::validate_lib(&lib)
                    .map_err(|_| RepositoryError::Metadata("dependency name"))?;
                if !seen.insert(lib.clone()) {
                    return Err(RepositoryError::Metadata("duplicate dependency"));
                }
                let versions = string(required(o, "versions")?)?
                    .parse()
                    .map_err(|_| RepositoryError::Metadata("dependency versions"))?;
                deps.push(RepoDependency { lib, versions });
            }
            Ok(deps)
        })
        .transpose()?;
    Ok(RepoLibVersion {
        pin,
        maturity: maturity(required(o, "maturity")?)?,
        availability,
        availability_msg: opt_text(o, "availabilityMsg")?,
        doc: opt_text(o, "doc")?,
        depends,
        digest: optional(o, "digest")
            .map(|v| Ok::<_, RepositoryError>(string(v)?.parse()?))
            .transpose()?,
    })
}
pub(super) fn versions(
    bytes: &[u8],
    l: &RepositoryLimits,
    lib: &str,
    filter: Option<&DependencyVersions>,
    limit: Option<usize>,
) -> Result<Vec<RepoLibVersion>, RepositoryError> {
    let v = json(bytes, l)?;
    let a = v
        .as_array()
        .ok_or(RepositoryError::Metadata("version list"))?;
    if a.len() > limit.unwrap_or(l.records) {
        return Err(RepositoryError::Limit("records"));
    }
    let mut records: Vec<RepoLibVersion> = Vec::new();
    for v in a {
        let row = record(v, l)?;
        if row.pin.lib() != lib {
            return Err(RepositoryError::Identity);
        }
        if records
            .last()
            .is_some_and(|prev| prev.pin.version() <= row.pin.version())
        {
            return Err(RepositoryError::Metadata(
                "versions not strictly newest first",
            ));
        }
        if filter.is_some_and(|f| !f.contains(row.pin.version())) {
            return Err(RepositoryError::Metadata("version outside requested range"));
        }
        records.push(row);
    }
    Ok(records)
}
pub(super) fn validate_record(
    row: &RepoLibVersion,
    l: &RepositoryLimits,
) -> Result<(), RepositoryError> {
    for text in [&row.doc, &row.availability_msg].into_iter().flatten() {
        if text.len() > l.string_bytes {
            return Err(RepositoryError::Limit("string bytes"));
        }
    }
    if let Some(deps) = &row.depends {
        if deps.len() > l.dependencies {
            return Err(RepositoryError::Limit("dependencies"));
        }
        let mut seen = BTreeSet::new();
        for dep in deps {
            types::validate_lib(&dep.lib)?;
            if !seen.insert(&dep.lib) {
                return Err(RepositoryError::InvalidInput("duplicate dependency"));
            }
        }
    }
    Ok(())
}
pub(super) fn api_error(status: u16, bytes: &[u8], l: &RepositoryLimits) -> RepositoryError {
    let parse = || -> Result<(u16, RepoErrorKind), RepositoryError> {
        let v = json(bytes, l)?;
        let o = object(&v)?;
        string(required(o, "dis")?)?;
        opt_text(o, "errTrace")?;
        let advisory = integer(required(o, "status")?, l)?;
        if !(100..=599).contains(&advisory) {
            return Err(RepositoryError::Metadata("error status"));
        }
        let advisory = advisory as u16;
        let kind = match string(required(o, "spec")?)? {
            "sys.repo::UnknownLibErr" => {
                types::validate_lib(string(required(o, "lib")?)?)
                    .map_err(|_| RepositoryError::Metadata("error library"))?;
                RepoErrorKind::UnknownLibrary
            }
            "sys.repo::UnknownLibVersionErr" => {
                types::validate_lib(string(required(o, "lib")?)?)
                    .map_err(|_| RepositoryError::Metadata("error library"))?;
                version(required(o, "version")?)?;
                RepoErrorKind::UnknownVersion
            }
            "sys.api::UnknownFuncErr" => {
                string(required(o, "funcName")?)?;
                RepoErrorKind::Unsupported
            }
            "sys.api::NotImplementedErr" => RepoErrorKind::Unsupported,
            _ => RepoErrorKind::Other,
        };
        Ok((advisory, kind))
    };
    match parse() {
        Ok((advisory_status, kind)) => RepositoryError::Api {
            status,
            advisory_status: Some(advisory_status),
            kind,
        },
        Err(error) => error,
    }
}
