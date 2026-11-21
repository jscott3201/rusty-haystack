//! Selected, pinned H5 reads. No H4 fallback or remote catalog admission.
use std::sync::OnceLock;

mod control;
pub use control::ReadControl;
#[cfg(test)]
mod tests;

use haystack_core::{
    codecs::jeto,
    data::HDict,
    kinds::{HRef, Kind},
    xeto::catalog::{ARGUMENTS, CallableContext, Catalog},
};

use crate::{ClientError, HaystackClient, transport::http::HttpTransport};

pub const MAX_IDS: usize = 1000;
pub const MAX_ID_BYTES: usize = 8192;
pub const MAX_WIRE_BYTES: usize = 1024 * 1024;
pub const MEDIA_TYPE: &str = "application/json;box=all";

/// Owned typed arguments. Ref display text is not part of lookup identity.
#[derive(Debug, Clone)]
pub enum ReadRequest {
    ById { id: Option<HRef>, checked: bool },
    ByIds { ids: Vec<HRef>, checked: bool },
}

/// A fitted pinned ApiErr. HTTP status is authoritative; payload.status is advisory.
/// Display deliberately excludes peer-controlled descriptions and payload values.
#[derive(Debug, thiserror::Error)]
#[error("H5 request failed (HTTP {status}, {spec})")]
pub struct ApiError {
    pub status: u16,
    pub spec: String,
    pub payload: HDict,
}

struct Profile {
    catalog: Catalog,
    one: CallableContext,
    many: CallableContext,
}
fn invalid(reason: &'static str) -> ClientError {
    ClientError::Codec(reason.into())
}
fn codec_error(error: jeto::Error<ClientError>, reason: &'static str) -> ClientError {
    match error {
        jeto::Error::Budget(error) => error,
        _ => invalid(reason),
    }
}
fn profile() -> Result<&'static Profile, ClientError> {
    static PROFILE: OnceLock<Result<Profile, ()>> = OnceLock::new();
    PROFILE
        .get_or_init(|| {
            let catalog = Catalog::load_http_pinned().map_err(|_| ())?;
            let context = |name| {
                CallableContext::new(&catalog, catalog.declaration(name).ok_or(())?).map_err(|_| ())
            };
            let one = context("sys.api::readById")?;
            let many = context("sys.api::readByIds")?;
            Ok(Profile { catalog, one, many })
        })
        .as_ref()
        .map_err(|_| invalid("pinned H5 context unavailable"))
}
impl ReadRequest {
    pub fn operation(&self) -> &'static str {
        match self {
            Self::ById { .. } => "sys.api::readById",
            Self::ByIds { .. } => "sys.api::readByIds",
        }
    }
    pub fn validate(&self) -> Result<(), ClientError> {
        let ids: &[HRef] = match self {
            Self::ById { id, .. } => id.as_slice(),
            Self::ByIds { ids, .. } => ids,
        };
        if ids.len() > MAX_IDS || ids.iter().any(|id| id.val.len() > MAX_ID_BYTES) {
            return Err(invalid("H5 read exceeds ID bounds"));
        }
        Ok(())
    }
    fn context<'a>(&self, profile: &'a Profile) -> &'a CallableContext {
        match self {
            Self::ById { .. } => &profile.one,
            Self::ByIds { .. } => &profile.many,
        }
    }
    pub(crate) fn encode(
        &self,
        control: &ReadControl,
        meter: &mut impl jeto::Meter<Error = ClientError>,
    ) -> Result<Vec<u8>, ClientError> {
        control.check()?;
        self.validate()?;
        let profile = profile()?;
        control.check()?;
        let context = self.context(profile);
        let mut args = HDict::new();
        let lookup = |id: &HRef| Kind::Ref(HRef::from_val(&id.val));
        let checked = match self {
            Self::ById { id, checked } => {
                if let Some(id) = id {
                    args.set("id", lookup(id));
                }
                *checked
            }
            Self::ByIds { ids, checked } => {
                let mut values = Vec::with_capacity(ids.len());
                for id in ids {
                    control.check()?;
                    values.push(lookup(id));
                }
                args.set("ids", Kind::List(values));
                *checked
            }
        };
        args.set("checked", Kind::Bool(checked));
        control.check()?;
        let fitted = profile
            .catalog
            .fit_arguments(self.operation(), &args)
            .map_err(|_| invalid("invalid pinned H5 arguments"));
        control.check()?;
        fitted?;
        let encoded = jeto::encode_metered(
            &Kind::Dict(Box::new(args)),
            &context.context,
            Some(ARGUMENTS),
            jeto::Boxing::All,
            meter,
        )
        .map_err(|error| codec_error(error, "invalid H5 request"));
        control.check()?;
        encoded?
            .into_exact()
            .map_err(|_| invalid("H5 arguments are not exactly representable"))
    }
    pub(crate) fn decode(
        &self,
        status: u16,
        bytes: &[u8],
        control: &ReadControl,
        meter: &mut impl jeto::Meter<Error = ClientError>,
    ) -> Result<Kind, ClientError> {
        control.check()?;
        let profile = profile()?;
        control.check()?;
        let context = self.context(profile);
        let success = (200..300).contains(&status);
        let decoded = jeto::decode_metered(
            bytes,
            &context.context,
            success.then_some(context.result.as_str()),
            meter,
        )
        .map_err(|error| codec_error(error, "invalid or unsupported H5 response"));
        control.check()?;
        let value = decoded?;
        if !success {
            let Kind::Dict(payload) = value else {
                return Err(invalid("invalid H5 error envelope"));
            };
            let Some(Kind::Ref(spec)) = payload.get("spec") else {
                return Err(invalid("missing H5 error spec"));
            };
            let spec = spec.val.clone();
            let mut fields = (*payload).clone();
            fields.remove_tag("spec");
            control.check()?;
            let fitted = profile
                .catalog
                .fit_api_error(&spec, &fields)
                .map_err(|_| invalid("invalid or unsupported H5 error envelope"));
            control.check()?;
            fitted?;
            return Err(ClientError::H5(ApiError {
                status,
                spec,
                payload: *payload,
            }));
        }
        let fitted = profile
            .catalog
            .fit_result(self.operation(), &value)
            .map_err(|_| invalid("H5 response does not fit the pinned result"));
        control.check()?;
        fitted?;
        // Bind returned entity identities and cardinality to this exact request.
        let found = |row: &HDict, id: &HRef| matches!(row.get("id"), Some(Kind::Ref(actual)) if actual.val == id.val);
        let matches = match (self, &value) {
            (Self::ById { checked: false, .. }, Kind::Null) => true,
            (Self::ById { id: Some(id), .. }, Kind::Dict(row)) => found(row, id),
            (Self::ByIds { ids, checked }, Kind::Grid(grid)) => {
                let mut matches = grid.rows.len() == ids.len();
                for (row, id) in grid.rows.iter().zip(ids) {
                    control.check()?;
                    matches &= found(row, id)
                        || (!checked && row.iter().all(|(_, value)| matches!(value, Kind::Null)));
                    if !matches {
                        break;
                    }
                }
                matches
            }
            _ => false,
        };
        control.check()?;
        if !matches {
            return Err(invalid("H5 response does not match requested identities"));
        }
        Ok(value)
    }
}

impl HaystackClient<HttpTransport> {
    /// Execute a bounded selected read against the original authenticated session.
    pub async fn read_h5(
        &self,
        request: &ReadRequest,
        control: &ReadControl,
    ) -> Result<Kind, ClientError> {
        control.check()?;
        let result = tokio::select! {
            biased;
            error = control.stopped() => Err(error),
            result = self.transport().call_h5(request, control) => result,
        };
        control.check()?;
        result
    }
}
