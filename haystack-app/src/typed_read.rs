//! Initial pinned readById wire profile. Complete Jeto and general dispatch are
//! deliberately separate work. All allocating stages share the read admission.
use crate::{BudgetKind, H4Codec, OutputProfile, ReadError, ReadOutput, budget::Budget, output};
use haystack_core::{
    codecs::codec_for,
    data::HDict,
    kinds::{HRef, Kind},
};
use serde_json::Value;

/// Fixed terminal envelopes have bounded allocation even after budget exhaustion.
/// Names and fields are the reachable subset of pinned sys.api::ApiErr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    InvalidArgs,
    UnsupportedVersion,
    UnsupportedMediaType,
    NotAcceptable,
    NotImplemented,
    InvalidPath,
    UnknownFunction(String),
    UnknownEntity,
    Permission,
    AuthRequired,
    AuthRejected,
    AuthMalformed,
    Timeout,
    Unavailable,
    Internal,
}
impl ApiError {
    pub fn status(&self) -> u16 {
        match self {
            Self::InvalidArgs | Self::UnsupportedVersion | Self::AuthMalformed => 400,
            Self::AuthRequired => 401,
            Self::AuthRejected | Self::Permission => 403,
            Self::InvalidPath | Self::UnknownEntity | Self::UnknownFunction(_) => 404,
            Self::NotAcceptable => 406,
            Self::UnsupportedMediaType => 415,
            Self::NotImplemented => 501,
            Self::Timeout => 504,
            Self::Unavailable => 503,
            Self::Internal => 500,
        }
    }
    pub fn json(&self) -> std::borrow::Cow<'static, str> {
        let text = match self {
            Self::UnknownFunction(name) => {
                // Bounded terminal encoding also works after deadline/budget
                // exhaustion. Names above this bound become InvalidPath.
                if name.len() > 256 {
                    return Self::InvalidPath.json();
                }
                return std::borrow::Cow::Owned(serde_json::json!({"spec":"sys.api::UnknownFuncErr","status":404,"dis":"Unknown function","funcName":name}).to_string());
            }
            Self::InvalidArgs => {
                r#"{"spec":"sys.api::InvalidArgsErr","status":400,"dis":"Invalid request arguments"}"#
            }
            Self::UnsupportedVersion => {
                r#"{"spec":"sys.api::UnsupportedVersionErr","status":400,"dis":"Unsupported protocol version","allow":["4","5"]}"#
            }
            Self::UnsupportedMediaType => {
                r#"{"spec":"sys.api::UnsupportedMediaTypeErr","status":415,"dis":"Unsupported request media type"}"#
            }
            Self::NotAcceptable => {
                r#"{"spec":"sys.api::NotAcceptableErr","status":406,"dis":"Unsupported response representation"}"#
            }
            Self::NotImplemented => {
                r#"{"spec":"sys.api::NotImplementedErr","status":501,"dis":"Capability not implemented by this profile"}"#
            }
            Self::InvalidPath => {
                r#"{"spec":"sys.api::InvalidPathErr","status":404,"dis":"Unknown API endpoint"}"#
            }
            Self::UnknownEntity => {
                r#"{"spec":"sys.api::UnknownEntityErr","status":404,"dis":"Entity unavailable"}"#
            }
            Self::Permission => {
                r#"{"spec":"sys.api::PermissionErr","status":403,"dis":"Operation forbidden"}"#
            }
            Self::AuthRequired => {
                r#"{"spec":"sys.api::AuthErr","status":401,"dis":"Authentication required"}"#
            }
            Self::AuthRejected => {
                r#"{"spec":"sys.api::AuthErr","status":403,"dis":"Authentication rejected"}"#
            }
            Self::AuthMalformed => {
                r#"{"spec":"sys.api::AuthErr","status":400,"dis":"Malformed authorization"}"#
            }
            Self::Timeout => {
                r#"{"spec":"sys.api::TimeoutErr","status":504,"dis":"Request deadline exceeded"}"#
            }
            Self::Unavailable => {
                r#"{"spec":"sys.api::UnavailableErr","status":503,"dis":"Service unavailable"}"#
            }
            Self::Internal => {
                r#"{"spec":"sys.api::InternalErr","status":500,"dis":"Internal error"}"#
            }
        };
        std::borrow::Cow::Borrowed(text)
    }
}
impl From<ReadError> for ApiError {
    fn from(error: ReadError) -> Self {
        match error {
            ReadError::Forbidden => Self::Permission,
            ReadError::Deadline => Self::Timeout,
            ReadError::Projection => Self::NotAcceptable,
            ReadError::InvalidQuery(_)
            | ReadError::Budget(_)
            | ReadError::UnitTooLarge
            | ReadError::StaleCursor => Self::InvalidArgs,
            ReadError::InvalidLimits => Self::Internal,
            _ => Self::Unavailable,
        }
    }
}

/// Raw transport data, reserved through `ReadAdmission::reserve_wire_input`
/// before its buffers are allocated. The worker resolves all protocol controls.
#[derive(Default)]
pub struct TypedReadInput {
    pub operation: String,
    pub post: bool,
    pub query: String,
    pub versions: Vec<String>,
    pub content_types: Vec<String>,
    pub accepts: Vec<String>,
    pub accept_encodings: Vec<String>,
    pub content_encoded: bool,
    pub body: Vec<u8>,
}
pub struct TypedReadResponse {
    pub body: Vec<u8>,
    pub content_type: &'static str,
    pub version: &'static str,
    pub gzip: bool,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Media {
    Jeto,
    Grid(H4Codec),
}
impl Media {
    fn content_type(self) -> &'static str {
        match self {
            Self::Jeto | Self::Grid(H4Codec::Json | H4Codec::JsonV3) => "application/json",
            Self::Grid(c) => c.mime(),
        }
    }
}
pub(crate) struct Request {
    pub args: HDict,
    pub version: &'static str,
    pub output: Media,
    pub gzip: bool,
}

fn percent(value: &str, budget: &mut Budget) -> Result<String, ApiError> {
    budget.charge(BudgetKind::Work, value.len().saturating_add(1))?;
    budget.charge(BudgetKind::Retained, value.len().saturating_add(32))?;
    let mut bytes = Vec::with_capacity(value.len());
    let mut iter = value.bytes();
    while let Some(byte) = iter.next() {
        bytes.push(match byte {
            b'+' => b' ',
            b'%' => {
                let a = iter
                    .next()
                    .and_then(|c| (c as char).to_digit(16))
                    .ok_or(ApiError::InvalidArgs)?;
                let b = iter
                    .next()
                    .and_then(|c| (c as char).to_digit(16))
                    .ok_or(ApiError::InvalidArgs)?;
                (a * 16 + b) as u8
            }
            c => c,
        });
    }
    String::from_utf8(bytes).map_err(|_| ApiError::InvalidArgs)
}
fn media(value: &str, v5: bool) -> Option<Media> {
    let mut split = value.split(';');
    let name = split.next()?.trim();
    let mut version = None;
    for part in split {
        let (key, value) = part.trim().split_once('=')?;
        if key.eq_ignore_ascii_case("box")
            && value == "auto"
            && matches!(name, "application/json" | "text/jeto")
            && v5
        {
            continue;
        }
        if key.eq_ignore_ascii_case("charset") && value.eq_ignore_ascii_case("utf-8") {
            continue;
        }
        if key.eq_ignore_ascii_case("version") && version.is_none() {
            version = Some(value.trim_matches('"'));
        } else {
            return None;
        }
    }
    if version.is_some_and(|v| v != "4") {
        return None;
    }
    match name {
        "application/json" if version.is_none() => Some(if v5 {
            Media::Jeto
        } else {
            Media::Grid(H4Codec::Json)
        }),
        "text/jeto" if v5 && version.is_none() => Some(Media::Jeto),
        "application/vnd.haystack+json" => Some(Media::Grid(H4Codec::Json)),
        "text/zinc" if version.is_none() => Some(Media::Grid(H4Codec::Zinc)),
        _ => None,
    }
}
fn response_media(
    input: &TypedReadInput,
    filetype: Option<&str>,
    v5: bool,
) -> Result<Media, ApiError> {
    if let Some(name) = filetype {
        return match name {
            "jeto" if v5 => Ok(Media::Jeto),
            "json" => Ok(if v5 {
                Media::Jeto
            } else {
                Media::Grid(H4Codec::Json)
            }),
            "hayson" => Ok(Media::Grid(H4Codec::Json)),
            "zinc" => Ok(Media::Grid(H4Codec::Zinc)),
            _ => Err(ApiError::NotAcceptable),
        };
    }
    let default = if v5 {
        Media::Jeto
    } else {
        Media::Grid(H4Codec::Zinc)
    };
    if input.accepts.is_empty() {
        return Ok(default);
    }
    // Resolve quality for each representation before comparing them. A broad
    // range must never override a more specific exclusion. Both JSON dialects
    // are sent as application/json, including when selected by a format alias.
    let candidates = [
        default,
        Media::Grid(H4Codec::Json),
        Media::Grid(H4Codec::Zinc),
        Media::Jeto,
    ];
    let mut selected = None;
    let mut best = 0.0_f32;
    for candidate in candidates {
        if candidate == Media::Jeto && !v5 {
            continue;
        }
        let mut effective: Option<(usize, f32)> = None;
        for header in &input.accepts {
            for entry in header.split(',') {
                let mut q = 1.0_f32;
                let mut media_end = entry.len();
                for (offset, _) in entry.match_indices(';') {
                    if let Some(value) = entry[offset + 1..].trim().strip_prefix("q=") {
                        q = value.parse().map_err(|_| ApiError::NotAcceptable)?;
                        media_end = offset;
                        break;
                    }
                }
                if !q.is_finite() || !(0.0..=1.0).contains(&q) {
                    return Err(ApiError::NotAcceptable);
                }
                let mime = entry[..media_end].trim();
                let specificity = if mime == "*/*" {
                    Some(0)
                } else if mime.strip_suffix("/*").is_some_and(|group| {
                    candidate
                        .content_type()
                        .split_once('/')
                        .is_some_and(|(kind, _)| kind == group)
                }) {
                    Some(1)
                } else if media(mime, v5) == Some(candidate)
                    || mime == "application/json" && candidate.content_type() == "application/json"
                {
                    Some(2 + mime.bytes().filter(|b| *b == b';').count())
                } else {
                    None
                };
                if let Some(specificity) = specificity
                    && effective.is_none_or(|(old, quality)| {
                        specificity > old || specificity == old && q > quality
                    })
                {
                    effective = Some((specificity, q));
                }
            }
        }
        if let Some((_, q)) = effective
            && q > best
        {
            best = q;
            selected = Some(candidate);
        }
    }
    selected.ok_or(ApiError::NotAcceptable)
}

fn argument(name: &str, value: &Value) -> Result<Option<Kind>, ApiError> {
    if value.is_null() {
        return Ok(None);
    }
    // Boxed scalar identity overrides context, then native fitting checks it.
    let (spec, value) = match value {
        Value::Object(map) => {
            if map
                .keys()
                .any(|k| !matches!(k.as_str(), "spec" | "val" | "dis"))
            {
                return Err(ApiError::InvalidArgs);
            }
            let spec = map
                .get("spec")
                .and_then(Value::as_str)
                .ok_or(ApiError::InvalidArgs)?;
            let val = map
                .get("val")
                .and_then(Value::as_str)
                .ok_or(ApiError::InvalidArgs)?;
            let result = match spec {
                "sys::Ref" => Kind::Ref(HRef::new(
                    val,
                    map.get("dis")
                        .map(|v| v.as_str().ok_or(ApiError::InvalidArgs))
                        .transpose()?
                        .map(str::to_owned),
                )),
                "sys::Bool" => Kind::Bool(match val {
                    "true" => true,
                    "false" => false,
                    _ => return Err(ApiError::InvalidArgs),
                }),
                "sys::Str" => Kind::Str(val.into()),
                _ => return Err(ApiError::InvalidArgs),
            };
            return Ok(Some(result));
        }
        value => (name, value),
    };
    Ok(Some(match (spec, value) {
        ("id", Value::String(s)) => Kind::Ref(HRef::from_val(s)),
        ("checked", Value::String(s)) if s == "true" || s == "false" => Kind::Bool(s == "true"),
        (_, Value::Bool(b)) => Kind::Bool(*b),
        (_, Value::String(s)) => Kind::Str(s.clone()),
        _ => return Err(ApiError::InvalidArgs),
    }))
}

pub(crate) fn decode(input: &TypedReadInput, budget: &mut Budget) -> Result<Request, ApiError> {
    let mut args = HDict::new();
    let mut query_args = Vec::new();
    let mut version = None;
    let mut filetype = None;
    for pair in input.query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = percent(key, budget)?;
        let value = percent(value, budget)?;
        match key.as_str() {
            "xeto-version" => {
                if version.replace(value).is_some() {
                    return Err(ApiError::UnsupportedVersion);
                }
            }
            "xeto-filetype" => {
                if filetype.replace(value).is_some() {
                    return Err(ApiError::NotAcceptable);
                }
            }
            key if key.starts_with("xeto-") => {}
            "id" | "checked" if !input.post => {
                if query_args.iter().any(|(name, _)| name == &key) {
                    return Err(ApiError::InvalidArgs);
                }
                budget.charge(BudgetKind::Retained, 128)?;
                query_args.push((key, value));
            }
            _ => {}
        }
    }
    if input.versions.len() > 1 {
        return Err(ApiError::UnsupportedVersion);
    }
    let version = version
        .as_deref()
        .or_else(|| input.versions.first().map(String::as_str))
        .unwrap_or("4");
    let version = match version {
        "4" => "4",
        "5" => "5",
        _ => return Err(ApiError::UnsupportedVersion),
    };
    let operation = percent(&input.operation, budget)?;
    if !matches!(operation.as_str(), "readById" | "sys.api::readById") {
        if operation.len() > 256
            || operation.is_empty()
            || !operation
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_:.".contains(&b))
        {
            return Err(ApiError::InvalidPath);
        }
        return Err(ApiError::UnknownFunction(operation));
    }
    let output = response_media(input, filetype.as_deref(), version == "5")?;
    let gzip = accepts_gzip(&input.accept_encodings)?;
    for (key, value) in query_args {
        budget.charge(
            BudgetKind::Retained,
            value.len().saturating_mul(512).saturating_add(1024),
        )?;
        let value = if value.starts_with(['[', '{']) {
            serde_json::from_str(&value).map_err(|_| ApiError::InvalidArgs)?
        } else {
            Value::String(value)
        };
        if let Some(value) = argument(&key, &value)? {
            args.set(key, value);
        }
    }
    if input.content_encoded {
        return Err(ApiError::UnsupportedMediaType);
    }
    if input.post {
        if input.content_types.len() != 1 {
            return Err(ApiError::UnsupportedMediaType);
        }
        let input_media =
            media(&input.content_types[0], version == "5").ok_or(ApiError::UnsupportedMediaType)?;
        let text = std::str::from_utf8(&input.body).map_err(|_| ApiError::InvalidArgs)?;
        let lines = text.lines().count().saturating_add(1);
        let longest = text.lines().map(str::len).max().unwrap_or(0);
        let expansion = text
            .len()
            .saturating_add(if input_media == Media::Grid(H4Codec::Zinc) {
                lines.saturating_mul(longest)
            } else {
                0
            });
        // Legacy Zinc builds every row and clones column names even though
        // argument binding consumes only the first row. Account for expanded
        // name-copy work before entering that unmetered decoder.
        budget.charge(BudgetKind::Work, expansion.saturating_add(1))?;
        budget.charge(
            BudgetKind::Retained,
            expansion.saturating_mul(512).saturating_add(2048),
        )?;
        if !text.trim().is_empty() {
            match input_media {
                Media::Jeto => {
                    let value: Value =
                        serde_json::from_str(text).map_err(|_| ApiError::InvalidArgs)?;
                    let map = value.as_object().ok_or(ApiError::InvalidArgs)?;
                    for name in ["id", "checked"] {
                        if let Some(value) = map.get(name)
                            && let Some(value) = argument(name, value)?
                        {
                            args.set(name, value);
                        }
                    }
                }
                Media::Grid(codec) => {
                    if codec == H4Codec::Json {
                        let envelope: Value =
                            serde_json::from_str(text).map_err(|_| ApiError::InvalidArgs)?;
                        if envelope.get("_kind").and_then(Value::as_str) != Some("grid")
                            || !envelope.get("meta").is_some_and(Value::is_object)
                            || !envelope.get("cols").is_some_and(Value::is_array)
                            || !envelope.get("rows").is_some_and(Value::is_array)
                        {
                            return Err(ApiError::InvalidArgs);
                        }
                    }
                    let grid = codec_for(codec.mime())
                        .ok_or(ApiError::Internal)?
                        .decode_grid(text)
                        .map_err(|_| ApiError::InvalidArgs)?;
                    if let Some(row) = grid.rows.first() {
                        for name in ["id", "checked"] {
                            if let Some(value) = row.get(name)
                                && !matches!(value, Kind::Null)
                            {
                                args.set(name, value.clone());
                            }
                        }
                    }
                }
            }
        }
    }
    budget.check()?;
    Ok(Request {
        args,
        version,
        output,
        gzip,
    })
}

// Conservative bound is computed before any serializer/scalar formatting. The
// reservation includes JSON nodes, scalar temporaries and the output buffer.
fn bound(value: &Kind, budget: &mut Budget, depth: usize) -> Result<usize, ApiError> {
    budget.depth(depth)?;
    budget.charge(BudgetKind::Values, 1)?;
    budget.charge(BudgetKind::Work, 1)?;
    let mut size = if matches!(value, Kind::Number(_)) {
        512_usize
    } else {
        256_usize
    };
    let string = |s: &str| s.len().saturating_mul(6).saturating_add(128);
    match value {
        Kind::Null | Kind::Bool(_) | Kind::Marker | Kind::None | Kind::NA | Kind::Int(_) => {}
        Kind::Float(f) if f.value().is_finite() => {}
        Kind::Number(n) if n.val.is_finite() => {
            if let Some(unit) = &n.unit {
                // The initial text encoding must preserve both unit identity and
                // the pinned unit grammar; Some("") would decode as unitless.
                if unit.is_empty()
                    || !unit
                        .chars()
                        .all(|c| !c.is_ascii() || c.is_ascii_alphabetic() || "%_/$".contains(c))
                {
                    return Err(ApiError::NotAcceptable);
                }
                size = size.saturating_add(string(unit));
            }
        }
        Kind::Str(s) => size = size.saturating_add(string(s)),
        Kind::Ref(r) => {
            size = size.saturating_add(string(&r.val));
            if let Some(dis) = &r.dis {
                size = size.saturating_add(string(dis));
            }
        }
        Kind::Buf(bytes) => size = size.saturating_add(bytes.len().saturating_mul(2)),
        Kind::List(values) => {
            for value in values {
                size = size.saturating_add(bound(value, budget, depth + 1)?);
            }
        }
        Kind::Dict(dict) => {
            for (key, value) in dict.iter() {
                // A null dict member disappears on Jeto decode; a spec member
                // is structural. Reject instead of silently changing identity.
                if key == "spec" || matches!(value, Kind::Null) {
                    return Err(ApiError::NotAcceptable);
                }
                size = size.saturating_add(string(key)).saturating_add(bound(
                    value,
                    budget,
                    depth + 1,
                )?);
            }
        }
        _ => return Err(ApiError::NotAcceptable),
    }
    if size > budget.limits.max_output_bytes {
        return Err(ApiError::InvalidArgs);
    }
    Ok(size)
}
fn boxed(spec: &str, val: String) -> Value {
    serde_json::json!({"spec":spec,"val":val})
}
fn json(value: &Kind) -> Value {
    use base64::Engine;
    match value {
        Kind::Null => Value::Null,
        Kind::Bool(v) => Value::Bool(*v),
        Kind::Str(v) => Value::String(v.clone()),
        Kind::Marker => boxed("sys::Marker", "✓".into()),
        Kind::None => boxed("sys::None", "∅".into()),
        Kind::NA => boxed("sys::NA", "NA".into()),
        Kind::Int(v) => Value::Number((*v).into()),
        Kind::Float(v) => {
            Value::Number(serde_json::Number::from_f64(v.value()).expect("finite preflight"))
        }
        Kind::Number(v) => boxed("sys::Number", v.to_string()),
        Kind::Buf(v) => boxed(
            "sys::Buf",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v),
        ),
        Kind::Ref(v) => {
            let mut val = boxed("sys::Ref", v.val.clone());
            if let Some(dis) = &v.dis {
                val["dis"] = Value::String(dis.clone());
            }
            val
        }
        Kind::List(values) => Value::Array(values.iter().map(json).collect()),
        Kind::Dict(dict) => {
            Value::Object(dict.iter().map(|(k, v)| (k.to_owned(), json(v))).collect())
        }
        _ => unreachable!("preflight rejects unsupported values"),
    }
}
pub(crate) fn encode(
    value: Kind,
    request: &Request,
    budget: &mut Budget,
) -> Result<TypedReadResponse, ApiError> {
    #[cfg(test)]
    if let Some(hook) = &budget.typed_encode_hook {
        hook();
    }
    let body = match request.output {
        Media::Jeto => {
            let bound = bound(&value, budget, 0)?;
            budget.charge(BudgetKind::Retained, bound.saturating_mul(8))?;
            let body = serde_json::to_vec(&json(&value)).map_err(|_| ApiError::Internal)?;
            budget.check()?;
            if body.len() > bound {
                return Err(ApiError::Internal);
            }
            body
        }
        Media::Grid(codec) => {
            let rows = match value {
                Kind::Dict(dict) => vec![*dict],
                Kind::Null => vec![],
                _ => return Err(ApiError::Internal),
            };
            let mut grid = output::grid(rows, true, None, budget)?;
            grid.meta.remove_tag("complete");
            let ReadOutput::H4 { body, .. } =
                output::encode(grid, OutputProfile::H4(codec), budget)?
            else {
                return Err(ApiError::Internal);
            };
            body
        }
    };
    finish(body, request, budget, request.version)
}
/// A function failure in v4 is an operation error grid with HTTP 200. Decode,
/// fitting, admission and media failures remain terminal ApiErr responses.
pub(crate) fn missing(
    request: &Request,
    budget: &mut Budget,
) -> Result<TypedReadResponse, ApiError> {
    if request.version == "5" {
        return Err(ApiError::UnknownEntity);
    }
    budget.charge(BudgetKind::Retained, 1024)?;
    let body = match request.output {
        Media::Grid(H4Codec::Zinc) => b"ver:\"3.0\" err dis:\"Entity unavailable\"\nempty\n".to_vec(),
        Media::Grid(H4Codec::Json) => br#"{"_kind":"grid","meta":{"ver":"3.0","err":{"_kind":"marker"},"dis":"Entity unavailable"},"cols":[{"name":"empty"}],"rows":[]}"#.to_vec(),
        _ => return Err(ApiError::NotAcceptable),
    };
    finish(body, request, budget, "5")
}

fn accepts_gzip(headers: &[String]) -> Result<bool, ApiError> {
    let mut gzip = None;
    let mut wildcard = None;
    let mut identity = None;
    for header in headers {
        for entry in header.split(',') {
            let mut parts = entry.trim().split(';');
            let token = parts.next().unwrap_or("").trim();
            let mut q = 1.0_f32;
            if let Some(parameter) = parts.next() {
                q = parameter
                    .trim()
                    .strip_prefix("q=")
                    .ok_or(ApiError::NotAcceptable)?
                    .parse()
                    .map_err(|_| ApiError::NotAcceptable)?;
            }
            if parts.next().is_some() || !q.is_finite() || !(0.0..=1.0).contains(&q) {
                return Err(ApiError::NotAcceptable);
            }
            if token.eq_ignore_ascii_case("gzip") {
                gzip = Some(q);
            } else if token == "*" {
                wildcard = Some(q);
            } else if token.eq_ignore_ascii_case("identity") {
                identity = Some(q);
            }
        }
    }
    if gzip.or(wildcard).is_some_and(|q| q > 0.0) {
        return Ok(true);
    }
    if identity == Some(0.0) || identity.is_none() && wildcard == Some(0.0) {
        return Err(ApiError::NotAcceptable);
    }
    Ok(false)
}
fn finish(
    body: Vec<u8>,
    request: &Request,
    budget: &mut Budget,
    version: &'static str,
) -> Result<TypedReadResponse, ApiError> {
    use std::io::Write;
    let body = if request.gzip {
        let bound = body.len().saturating_mul(2).saturating_add(1024);
        if bound > budget.limits.max_output_bytes {
            return Err(ApiError::NotAcceptable);
        }
        budget.charge(BudgetKind::Work, body.len())?;
        // Includes the compressor's bounded state, temporary blocks and output.
        budget.charge(
            BudgetKind::Retained,
            bound.saturating_mul(4).saturating_add(1024 * 1024),
        )?;
        let mut encoder = flate2::write::GzEncoder::new(
            Vec::with_capacity(bound),
            flate2::Compression::default(),
        );
        encoder.write_all(&body).map_err(|_| ApiError::Internal)?;
        let compressed = encoder.finish().map_err(|_| ApiError::Internal)?;
        budget.check()?;
        if compressed.len() > bound {
            return Err(ApiError::Internal);
        }
        compressed
    } else {
        body
    };
    budget.check()?;
    Ok(TypedReadResponse {
        body,
        content_type: request.output.content_type(),
        version,
        gzip: request.gzip,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AllowAll, ApplicationBuilder, CancellationToken, Principal, ReadContext, ReadLimits,
        ReadService, ShutdownPolicy,
    };
    use haystack_core::{
        graph::{EntityGraph, SharedGraph},
        xeto::read_by_id::ReadByIdProfile,
    };
    use std::{future::Future, sync::Arc, task::Poll, time::Duration};
    fn input() -> TypedReadInput {
        TypedReadInput {
            operation: "readById".into(),
            versions: vec!["5".into()],
            query: "checked=false".into(),
            ..Default::default()
        }
    }
    #[test]
    fn every_terminal_envelope_fits_the_retained_reachable_error_declaration() {
        let profile = ReadByIdProfile::load_http_pinned().unwrap();
        for error in [
            ApiError::InvalidArgs,
            ApiError::UnsupportedVersion,
            ApiError::UnsupportedMediaType,
            ApiError::NotAcceptable,
            ApiError::NotImplemented,
            ApiError::InvalidPath,
            ApiError::UnknownFunction("absent".into()),
            ApiError::UnknownEntity,
            ApiError::Permission,
            ApiError::AuthRequired,
            ApiError::AuthRejected,
            ApiError::AuthMalformed,
            ApiError::Timeout,
            ApiError::Unavailable,
            ApiError::Internal,
        ] {
            let value: Value = serde_json::from_str(&error.json()).unwrap();
            let mut fields = HDict::new();
            for (name, value) in value.as_object().unwrap() {
                let kind = match (name.as_str(), value) {
                    ("spec", _) => continue,
                    (_, Value::String(s)) => Kind::Str(s.clone()),
                    (_, Value::Number(n)) => Kind::Int(n.as_i64().unwrap()),
                    (_, Value::Array(values)) => Kind::List(
                        values
                            .iter()
                            .map(|v| Kind::Str(v.as_str().unwrap().to_owned()))
                            .collect(),
                    ),
                    _ => panic!("unadmitted field"),
                };
                fields.set(name, kind);
            }
            assert_eq!(
                fields.get("status"),
                Some(&Kind::Int(i64::from(error.status())))
            );
            assert!(!fields.has("errTrace"));
            profile
                .fit_api_error(value["spec"].as_str().unwrap(), &fields)
                .unwrap();
        }
    }
    #[tokio::test]
    async fn transport_reservations_and_encoding_use_one_cumulative_budget() {
        let reads = ReadService::new(
            SharedGraph::new(EntityGraph::new()),
            Arc::new(AllowAll),
            ReadLimits {
                max_retained_bytes: 12_288,
                ..ReadLimits::default()
            },
        )
        .unwrap();
        let context = || ReadContext::with_timeout(Principal::Anonymous, Duration::from_secs(1));
        let plain = reads
            .begin(context())
            .await
            .unwrap()
            .read_by_id_wire(input())
            .await
            .unwrap();
        assert_eq!(plain.body, b"null");
        let mut admission = reads.begin(context()).await.unwrap();
        // Still enough for argument decode/binding; the final encoder's
        // reservation crosses the original cumulative ceiling.
        admission.reserve_wire_input(512).unwrap();
        assert!(matches!(
            admission.read_by_id_wire(input()).await,
            Err(ApiError::InvalidArgs)
        ));
        assert_eq!(reads.load().admitted, 0);
    }
    #[test]
    fn cancelling_during_typed_encoding_keeps_the_slot_and_owner_registration_until_exit() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let app = ApplicationBuilder::new(
                SharedGraph::new(EntityGraph::new()),
                Arc::new(AllowAll),
                ReadLimits {
                    max_concurrent: 1,
                    max_queued: 0,
                    ..ReadLimits::default()
                },
            )
            .unwrap()
            .shutdown_policy(ShutdownPolicy {
                drain_timeout: Duration::ZERO,
                stop_timeout: Duration::from_secs(1),
            });
            let reads = app.handle().read_service();
            let owner = app.start(&tokio::runtime::Handle::current()).unwrap();
            owner.ready().await.unwrap();
            let cancel = CancellationToken::new();
            let mut admission = reads
                .begin(ReadContext::new(
                    Principal::Anonymous,
                    std::time::Instant::now() + Duration::from_secs(3),
                    cancel.clone(),
                ))
                .await
                .unwrap();
            let (entered_tx, entered_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let release = std::sync::Mutex::new(release_rx);
            admission.budget_mut().typed_encode_hook = Some(Arc::new(move || {
                entered_tx.send(()).unwrap();
                release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap();
            }));
            let mut result = Box::pin(admission.read_by_id_wire(input()));
            assert!(
                std::future::poll_fn(|cx| Poll::Ready(result.as_mut().poll(cx)))
                    .await
                    .is_pending()
            );
            entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
            cancel.cancel();
            assert!(matches!(result.await, Err(ApiError::Unavailable)));
            assert_eq!(reads.load().admitted, 1);
            let mut closing = Box::pin(owner.close());
            assert!(
                std::future::poll_fn(|cx| Poll::Ready(closing.as_mut().poll(cx)))
                    .await
                    .is_pending()
            );
            assert_eq!(reads.load().admitted, 1);
            release_tx.send(()).unwrap();
            tokio::task::spawn_blocking(|| ()).await.unwrap();
            closing.await.unwrap();
            owner.terminated().await;
            assert_eq!(reads.load().admitted, 0);
        });
    }
}
