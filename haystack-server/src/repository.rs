//! Explicit GET-only immutable repository adapter, separate from app dispatch.
use crate::state::SharedState;
use axum::{
    body::Body,
    extract::{Query, State},
    http::{Request, StatusCode, header},
    response::{IntoResponse, Response},
};
use haystack_app::ApiError;
use haystack_core::xeto::repository::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;
fn json_response(status: u16, value: Value) -> Response {
    (
        StatusCode::from_u16(status).unwrap(),
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::HeaderName::from_static("xeto-version"), "5"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        value.to_string(),
    )
        .into_response()
}
fn error(error: ApiError) -> Response {
    crate::typed_http::error(error)
}
fn maturity(value: Maturity) -> &'static str {
    match value {
        Maturity::Alpha => "alpha",
        Maturity::Beta => "beta",
        Maturity::Stable => "stable",
    }
}
fn metadata(package: &LibraryPackage) -> Value {
    let meta = package.metadata();
    json!({"lib":meta.pin.lib(),"version":meta.pin.version().to_string(),"maturity":maturity(meta.maturity),"availability":"available","doc":meta.doc,"depends":package.dependencies().iter().map(|pin|json!({"lib":pin.lib(),"versions":pin.version().to_string()})).collect::<Vec<_>>(),"digest":package.digest().to_string()})
}
fn single_header<'a>(request: &'a Request<Body>, name: &str) -> Result<Option<&'a str>, ApiError> {
    let mut values = request.headers().get_all(name).iter();
    let value = values
        .next()
        .map(|v| v.to_str().map_err(|_| ApiError::InvalidArgs))
        .transpose()?;
    if values.next().is_some() || value.is_some_and(|v| v.len() > 1024) {
        return Err(ApiError::InvalidArgs);
    }
    Ok(value)
}
fn controls(request: &Request<Body>) -> Result<BTreeMap<String, String>, ApiError> {
    if request
        .uri()
        .query()
        .is_some_and(|query| query.len() > 4096)
    {
        return Err(ApiError::InvalidArgs);
    }
    // The form decoder replaces malformed UTF-8; reject it before decoding.
    let raw = request.uri().query().unwrap_or("").as_bytes();
    let mut decoded = Vec::with_capacity(raw.len());
    let mut at = 0;
    while at < raw.len() {
        if raw[at] == b'%' {
            let hi = raw
                .get(at + 1)
                .and_then(|b| (*b as char).to_digit(16))
                .ok_or(ApiError::InvalidArgs)?;
            let lo = raw
                .get(at + 2)
                .and_then(|b| (*b as char).to_digit(16))
                .ok_or(ApiError::InvalidArgs)?;
            decoded.push((hi * 16 + lo) as u8);
            at += 3;
        } else {
            decoded.push(raw[at]);
            at += 1;
        }
    }
    std::str::from_utf8(&decoded).map_err(|_| ApiError::InvalidArgs)?;
    let Query(pairs) = Query::<Vec<(String, String)>>::try_from_uri(request.uri())
        .map_err(|_| ApiError::InvalidArgs)?;
    if pairs.len() > 8 {
        return Err(ApiError::InvalidArgs);
    }
    let mut params = BTreeMap::new();
    for (key, value) in pairs {
        if key.len() > 128 || value.len() > 1024 || params.insert(key, value).is_some() {
            return Err(ApiError::InvalidArgs);
        }
    }
    let header_version = single_header(request, "xeto-version")?;
    let version = params.remove("xeto-version");
    if version.as_deref().or(header_version).unwrap_or("5") != "5" {
        return Err(ApiError::UnsupportedVersion);
    }
    // Conditional/range downloads are outside the selected unconditional profile.
    for name in [
        "if-match",
        "if-none-match",
        "if-modified-since",
        "if-unmodified-since",
        "if-range",
        "range",
    ] {
        if request.headers().contains_key(name) {
            return Err(ApiError::InvalidArgs);
        }
    }
    Ok(params)
}
pub(crate) async fn handle(State(state): State<SharedState>, request: Request<Body>) -> Response {
    let Some(repository) = &state.repository else {
        return error(ApiError::InvalidPath);
    };
    let operation = request.uri().path().trim_start_matches('/');
    let mut params = match controls(&request) {
        Ok(params) => params,
        Err(ApiError::UnsupportedVersion) => {
            return json_response(
                400,
                json!({"spec":"sys.api::UnsupportedVersionErr","status":400,"dis":"Repository requires version 5","allow":["5"]}),
            );
        }
        Err(e) => return error(e),
    };
    if operation == "sys.repo::repoPublish" {
        return error(ApiError::NotImplemented);
    }
    if ![
        "sys.repo::repoPing",
        "sys.repo::repoSearch",
        "sys.repo::repoVersions",
        "sys.repo::repoFetch",
    ]
    .contains(&operation)
    {
        return error(ApiError::UnknownFunction(
            operation.chars().take(256).collect(),
        ));
    }
    if request.method() != axum::http::Method::GET {
        let mut response = json_response(
            405,
            json!({"spec":"sys.api::MethodNotAllowedErr","status":405,"dis":"Repository profile requires GET","allow":["GET"]}),
        );
        response
            .headers_mut()
            .insert(header::ALLOW, "GET".parse().unwrap());
        return response;
    }
    if single_header(&request, "content-length")
        .unwrap_or(Some("invalid"))
        .is_some_and(|s| s != "0")
        || request.headers().contains_key("transfer-encoding")
        || request.headers().contains_key("content-encoding")
    {
        return error(ApiError::InvalidArgs);
    }
    let filetype = params.remove("xeto-filetype");
    if filetype.as_deref().is_some_and(|value| value != "json") {
        return error(ApiError::NotAcceptable);
    }
    if operation == "sys.repo::repoFetch" {
        if filetype.is_some() {
            return error(ApiError::InvalidArgs);
        }
        return fetch(repository, params);
    }
    if filetype.is_none() {
        match single_header(&request, "accept") {
            Ok(None | Some("application/json" | "application/json;box=none" | "*/*")) => {}
            _ => return error(ApiError::NotAcceptable),
        }
    }
    let result = (|| -> Result<Value, ApiError> {
        let limit = if let Some(value) = params.remove("limit") {
            if !matches!(operation, "sys.repo::repoSearch" | "sys.repo::repoVersions")
                || value.is_empty()
                || !value.bytes().all(|b| b.is_ascii_digit())
            {
                return Err(ApiError::InvalidArgs);
            }
            let n = value.parse::<usize>().map_err(|_| ApiError::InvalidArgs)?;
            if n == 0 || n > 64 {
                return Err(ApiError::InvalidArgs);
            }
            n
        } else {
            64
        };
        let result = match operation {
            "sys.repo::repoPing" => json!({"dis":repository.display_name()}),
            "sys.repo::repoSearch" => {
                let query = params.remove("query").ok_or(ApiError::InvalidArgs)?;
                let mut libs: BTreeMap<&str, Vec<&LibraryPackage>> = BTreeMap::new();
                for package in repository.packages() {
                    if query == "*" || package.pin().lib().contains(&query) {
                        libs.entry(package.pin().lib()).or_default().push(package);
                    }
                }
                let total = libs.len();
                let summaries=libs.into_iter().take(limit).map(|(name,versions)|{
                    let latest=versions.last().unwrap();
                    let stable=versions.iter().rev().find(|p|p.metadata().maturity==Maturity::Stable).map(|p|p.pin().version().to_string());
                    json!({"lib":name,"latestVersion":latest.pin().version().to_string(),"latestMaturity":maturity(latest.metadata().maturity),"latestStable":stable,"doc":latest.metadata().doc})
                }).collect::<Vec<_>>();
                let mut out = json!({"libs":summaries,"total":total});
                if total > limit {
                    out["more"] = json!("✓");
                }
                out
            }
            "sys.repo::repoVersions" => {
                let lib = params.remove("lib").ok_or(ApiError::InvalidArgs)?;
                validate_lib(&lib).map_err(|_| ApiError::InvalidArgs)?;
                let versions = params
                    .remove("versions")
                    .map(|v| v.parse::<DependencyVersions>())
                    .transpose()
                    .map_err(|_| ApiError::InvalidArgs)?;
                json!(
                    repository
                        .packages()
                        .rev()
                        .filter(|p| p.pin().lib() == lib
                            && versions
                                .as_ref()
                                .is_none_or(|v| v.contains(p.pin().version())))
                        .take(limit)
                        .map(metadata)
                        .collect::<Vec<_>>()
                )
            }
            _ => return Err(ApiError::Internal),
        };
        if !params.is_empty() {
            return Err(ApiError::InvalidArgs);
        }
        Ok(result)
    })();
    match result {
        Ok(value) => json_response(200, value),
        Err(e) => error(e),
    }
}
fn fetch(repository: &LibraryRepository, mut params: BTreeMap<String, String>) -> Response {
    // Fetch is handled separately so Accept never changes exact archive bytes.
    let Some(lib) = params.remove("lib") else {
        return error(ApiError::InvalidArgs);
    };
    let Some(version) = params.remove("version") else {
        return error(ApiError::InvalidArgs);
    };
    if !params.is_empty() {
        return error(ApiError::InvalidArgs);
    }
    let pin = match version
        .parse()
        .and_then(|version| LibraryPin::new(&lib, version))
    {
        Ok(pin) => pin,
        Err(_) => return error(ApiError::InvalidArgs),
    };
    match repository.get(&pin) {
        Some(package) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/xetolib"),
                (header::HeaderName::from_static("xeto-version"), "5"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            package.bytes().to_vec(),
        )
            .into_response(),
        None if repository.packages().any(|p| p.pin().lib() == lib) => json_response(
            404,
            json!({"spec":"sys.repo::UnknownLibVersionErr","status":404,"dis":"Unknown library version","lib":lib,"version":pin.version().to_string()}),
        ),
        None => json_response(
            404,
            json!({"spec":"sys.repo::UnknownLibErr","status":404,"dis":"Unknown library","lib":lib}),
        ),
    }
}
