//! Pinned readById HTTP profile using the core contextual Jeto codec.
//! Codec context is derived from the admitted signature at service setup;
//! every allocating request stage shares the original read admission.
use crate::{BudgetKind, H4Codec, OutputProfile, ReadError, ReadOutput, budget::Budget, output};
use haystack_core::{
    codecs::{codec_for, jeto},
    data::HDict,
    kinds::Kind,
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
    Jeto(jeto::Boxing),
    Grid(H4Codec),
}
impl Media {
    fn content_type(self) -> &'static str {
        match self {
            Self::Jeto(_) | Self::Grid(H4Codec::Json | H4Codec::JsonV3) => "application/json",
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
    let mut boxing = None;
    for part in split {
        let (key, value) = part.trim().split_once('=')?;
        let value = value.trim_matches('"');
        if key.eq_ignore_ascii_case("box") && matches!(name, "application/json" | "text/jeto") && v5
        {
            let mode = match value {
                "auto" => jeto::Boxing::Auto,
                "none" => jeto::Boxing::None,
                "all" => jeto::Boxing::All,
                _ => return None,
            };
            if boxing.replace(mode).is_some() {
                return None;
            }
            continue;
        }
        if key.eq_ignore_ascii_case("charset") && value.eq_ignore_ascii_case("utf-8") {
            continue;
        }
        if key.eq_ignore_ascii_case("version") && version.is_none() {
            version = Some(value);
        } else {
            return None;
        }
    }
    if version.is_some_and(|v| v != "4") {
        return None;
    }
    match name {
        "application/json" if version.is_none() => Some(if v5 {
            Media::Jeto(boxing.unwrap_or_default())
        } else {
            Media::Grid(H4Codec::Json)
        }),
        "text/jeto" if v5 && version.is_none() => Some(Media::Jeto(boxing.unwrap_or_default())),
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
            "jeto" if v5 => Ok(Media::Jeto(jeto::Boxing::Auto)),
            "json" => Ok(if v5 {
                Media::Jeto(jeto::Boxing::Auto)
            } else {
                Media::Grid(H4Codec::Json)
            }),
            "hayson" => Ok(Media::Grid(H4Codec::Json)),
            "zinc" => Ok(Media::Grid(H4Codec::Zinc)),
            _ => Err(ApiError::NotAcceptable),
        };
    }
    let default = if v5 {
        Media::Jeto(jeto::Boxing::Auto)
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
        Media::Jeto(jeto::Boxing::Auto),
        Media::Jeto(jeto::Boxing::None),
        Media::Jeto(jeto::Boxing::All),
    ];
    let mut selected = None;
    let mut best = 0.0_f32;
    for candidate in candidates {
        if matches!(candidate, Media::Jeto(_)) && !v5 {
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

const ARGUMENTS: &str = "rusty.http::ReadByIdArgs";
/// A codec-only argument container derived from the actual admitted function
/// slots. It does not register a function or admit a broader Xeto catalog.
pub(crate) struct WireProfile {
    context: jeto::Context,
    parameters: std::collections::BTreeMap<String, String>,
    result: String,
}
impl WireProfile {
    pub(crate) fn new(
        profile: &haystack_core::xeto::read_by_id::ReadByIdProfile,
    ) -> Result<Self, ReadError> {
        let declaration = profile
            .declaration("sys.api::readById")
            .ok_or(ReadError::InvalidLimits)?;
        let mut parameters = std::collections::BTreeMap::new();
        let mut result = None;
        for slot in &declaration.spec.slots {
            let name = slot.type_ref.as_ref().ok_or(ReadError::InvalidLimits)?;
            if slot.name == "returns" {
                result = Some(name.clone());
            } else {
                parameters.insert(slot.name.clone(), name.clone());
            }
        }
        let context = jeto::Context::new(
            &profile.provenance().repository,
            &profile.provenance().commit,
            vec![jeto::Definition::Dict {
                name: ARGUMENTS.into(),
                members: parameters.clone(),
            }],
        )
        .map_err(|_| ReadError::InvalidLimits)?;
        let result = result.ok_or(ReadError::InvalidLimits)?;
        if !context.contains(&result) {
            return Err(ReadError::InvalidLimits);
        }
        Ok(Self {
            context,
            parameters,
            result,
        })
    }
}
struct JetoMeter<'a>(&'a mut Budget);
impl jeto::Meter for JetoMeter<'_> {
    type Error = ReadError;
    fn charge(&mut self, cost: jeto::Charge) -> Result<(), ReadError> {
        use jeto::Charge;
        self.0.check()?;
        match cost {
            // Raw transport bytes were already reserved before collection. The
            // core parser reports the document length for an additional ceiling
            // check, without renewing or double-charging that input allowance.
            Charge::Input(n) if n > self.0.limits.max_input_bytes => {
                Err(ReadError::Budget(BudgetKind::Input))
            }
            Charge::Output(n) if n > self.0.limits.max_output_bytes => {
                Err(ReadError::Budget(BudgetKind::Output))
            }
            Charge::Input(_) | Charge::Output(_) => Ok(()),
            Charge::Work(n) => self.0.charge(BudgetKind::Work, n),
            Charge::Retained(n) => self.0.charge(BudgetKind::Retained, n),
            Charge::Nodes(n) => self.0.charge(BudgetKind::Values, n),
            Charge::Depth(n) => self.0.depth(n),
        }
    }
}
fn codec_error(error: jeto::Error<ReadError>, output: bool) -> ApiError {
    match error {
        jeto::Error::Budget(error) => error.into(),
        jeto::Error::Allocation => ApiError::Unavailable,
        _ if output => ApiError::NotAcceptable,
        _ => ApiError::InvalidArgs,
    }
}
fn argument(
    value: &str,
    expected: &str,
    profile: &WireProfile,
    budget: &mut Budget,
) -> Result<Kind, ApiError> {
    if value.starts_with(['[', '{']) {
        jeto::decode_metered(
            value.as_bytes(),
            &profile.context,
            Some(expected),
            &mut JetoMeter(budget),
        )
        .map_err(|e| codec_error(e, false))
    } else {
        // GET non-container values are scalar text, even when they resemble
        // JSON numbers or null. Decode directly under the original budget.
        jeto::decode_scalar_text_metered(value, &profile.context, expected, &mut JetoMeter(budget))
            .map_err(|e| codec_error(e, false))
    }
}

pub(crate) fn decode(
    input: &TypedReadInput,
    profile: &WireProfile,
    budget: &mut Budget,
) -> Result<Request, ApiError> {
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
            name if !input.post && profile.parameters.contains_key(name) => {
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
        let expected = profile.parameters.get(&key).ok_or(ApiError::Internal)?;
        let value = argument(&value, expected, profile, budget)?;
        if !matches!(value, Kind::Null) {
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
        budget.charge(BudgetKind::Work, input.body.len().saturating_add(1))?;
        let text = std::str::from_utf8(&input.body).map_err(|_| ApiError::InvalidArgs)?;
        if !text.trim().is_empty() {
            match input_media {
                Media::Jeto(_) => {
                    let decoded = jeto::decode_metered(
                        &input.body,
                        &profile.context,
                        Some(ARGUMENTS),
                        &mut JetoMeter(budget),
                    )
                    .map_err(|e| codec_error(e, false))?;
                    let Kind::Dict(mut map) = decoded else {
                        return Err(ApiError::InvalidArgs);
                    };
                    for name in profile.parameters.keys() {
                        if let Some(value) = map.remove_tag(name) {
                            args.set(name, value);
                        }
                    }
                }
                Media::Grid(codec) => {
                    let lines = text.lines().count().saturating_add(1);
                    let longest = text.lines().map(str::len).max().unwrap_or(0);
                    let expansion = text.len().saturating_add(if codec == H4Codec::Zinc {
                        lines.saturating_mul(longest)
                    } else {
                        0
                    });
                    // Legacy Zinc clones column names for all rows. Keep its
                    // expanded reservation before entering the H4 decoder.
                    budget.charge(BudgetKind::Work, expansion.saturating_add(1))?;
                    budget.charge(
                        BudgetKind::Retained,
                        expansion.saturating_mul(512).saturating_add(2048),
                    )?;
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
                        for name in profile.parameters.keys() {
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

pub(crate) fn encode(
    value: Kind,
    request: &Request,
    profile: &WireProfile,
    budget: &mut Budget,
) -> Result<TypedReadResponse, ApiError> {
    #[cfg(test)]
    if let Some(hook) = &budget.typed_encode_hook {
        hook();
    }
    let body = match request.output {
        Media::Jeto(boxing) => {
            // This HTTP profile serves only exact output, including box=none.
            // Callers that deliberately want lossy unboxing use the core API's
            // explicit Encoding::Lossy result and its path-specific assessment.
            jeto::encode_metered(
                &value,
                &profile.context,
                Some(&profile.result),
                boxing,
                &mut JetoMeter(budget),
            )
            .map_err(|e| codec_error(e, true))?
            .into_exact()
            .map_err(|_| ApiError::NotAcceptable)?
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
        let service = |limits| {
            ReadService::new(
                SharedGraph::new(EntityGraph::new()),
                Arc::new(AllowAll),
                limits,
            )
            .unwrap()
        };
        let context = || ReadContext::with_timeout(Principal::Anonymous, Duration::from_secs(1));
        // Measure the complete request's budget reservations. This is an
        // accounting receipt, not a heap allocation/performance measurement.
        let controls = service(ReadLimits::default());
        let mut request = controls.begin(context()).await.unwrap();
        let observer = request.budget_mut().clone();
        assert_eq!(
            request.read_by_id_wire(input()).await.unwrap().body,
            b"null"
        );
        let request_cost = observer.limits.max_retained_bytes - observer.retained_remaining();
        drop(observer);
        let mut transport = controls.begin(context()).await.unwrap();
        let before = transport.budget_mut().retained_remaining();
        transport.reserve_wire_input(512).unwrap();
        let transport_cost = before - transport.budget_mut().retained_remaining();
        drop(transport);
        assert_eq!(controls.load().admitted, 0);
        let ceiling = request_cost.max(transport_cost) + 1;
        assert!(request_cost + transport_cost > ceiling);
        let reads = service(ReadLimits {
            max_retained_bytes: ceiling,
            ..ReadLimits::default()
        });
        // Each component fits by itself, while combined transport and later
        // request/codec reservations exhaust the same cumulative allowance.
        assert_eq!(
            reads
                .begin(context())
                .await
                .unwrap()
                .read_by_id_wire(input())
                .await
                .unwrap()
                .body,
            b"null"
        );
        let mut admission = reads.begin(context()).await.unwrap();
        admission.reserve_wire_input(512).unwrap();
        assert!(matches!(
            admission.read_by_id_wire(input()).await,
            Err(ApiError::InvalidArgs)
        ));
        assert_eq!(reads.load().admitted, 0);
    }
    #[test]
    fn core_codec_meter_preserves_original_interruptions_and_generated_output_limits() {
        use std::time::Instant;
        let profile = WireProfile::new(&ReadByIdProfile::load_http_pinned().unwrap()).unwrap();
        let fresh = |limits| {
            Budget::new(
                Arc::new(limits),
                Instant::now() + Duration::from_secs(1),
                CancellationToken::new(),
            )
        };
        let mut cancelled = fresh(ReadLimits::default());
        cancelled.cancel.cancel();
        let error = jeto::decode_metered(
            b"null",
            &profile.context,
            None,
            &mut JetoMeter(&mut cancelled),
        )
        .unwrap_err();
        assert!(matches!(error, jeto::Error::Budget(ReadError::Cancelled)));
        assert_eq!(codec_error(error, false), ApiError::Unavailable);
        let mut expired = fresh(ReadLimits::default());
        expired.deadline = Instant::now();
        let error = jeto::encode_metered(
            &Kind::Bool(true),
            &profile.context,
            None,
            jeto::Boxing::All,
            &mut JetoMeter(&mut expired),
        )
        .unwrap_err();
        assert!(matches!(error, jeto::Error::Budget(ReadError::Deadline)));
        assert_eq!(codec_error(error, true), ApiError::Timeout);
        let native = Kind::List(vec![Kind::Bool(true); 64]);
        for (limits, expected) in [
            (
                ReadLimits {
                    max_value_depth: 1,
                    ..ReadLimits::default()
                },
                BudgetKind::Depth,
            ),
            (
                ReadLimits {
                    max_value_nodes: 64 * 3,
                    ..ReadLimits::default()
                },
                BudgetKind::Values,
            ),
            (
                ReadLimits {
                    max_output_bytes: 16,
                    ..ReadLimits::default()
                },
                BudgetKind::Output,
            ),
        ] {
            let mut budget = fresh(limits);
            let error = jeto::encode_metered(
                &native,
                &profile.context,
                None,
                jeto::Boxing::All,
                &mut JetoMeter(&mut budget),
            )
            .unwrap_err();
            assert!(
                matches!(error, jeto::Error::Budget(ReadError::Budget(found)) if found == expected)
            );
            assert_eq!(codec_error(error, true), ApiError::InvalidArgs);
        }
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
