use haystack_app::postgres::{PostgresConfig, PostgresLimits, PostgresTransport};
use serde::Deserialize;
use std::{io::Read, path::Path};
pub(crate) struct Loaded {
    pub config: PostgresConfig,
    pub selected: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    version: u32,
    host: String,
    port: u16,
    database: String,
    username: String,
    schema: Option<String>,
    selector_schema: Option<String>,
    password_env: String,
    transport: Transport,
}

#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case", deny_unknown_fields)]
enum Transport {
    VerifyFull { additional_ca_file: Option<String> },
    LoopbackPlaintext,
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(limit + 1).read_to_end(&mut bytes))
        .map_err(|_| "unreadable PostgreSQL configuration or CA file")?;
    if bytes.len() as u64 > limit {
        return Err("PostgreSQL configuration or CA file exceeds size limit".into());
    }
    Ok(bytes)
}

pub(crate) fn load(path: &Path) -> Result<Loaded, String> {
    let bytes = read_bounded(path, 65_536)?;
    let source =
        std::str::from_utf8(&bytes).map_err(|_| "invalid PostgreSQL TOML configuration")?;
    // Do not return TOML errors: they can quote credentials or complete source lines.
    let config: Configuration =
        toml::from_str(source).map_err(|_| "invalid PostgreSQL TOML configuration")?;
    if config.version != 1 {
        return Err("unsupported PostgreSQL configuration version".into());
    }
    let (schema, selected) = match (config.schema, config.selector_schema) {
        (Some(schema), None) => (schema, false),
        (None, Some(schema)) => (schema, true),
        _ => return Err("choose exactly one PostgreSQL schema or selector_schema".into()),
    };
    if config.password_env.is_empty()
        || !config
            .password_env
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err("invalid PostgreSQL password environment variable name".into());
    }
    let password = std::env::var(&config.password_env)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or("PostgreSQL password environment variable is missing, empty or non-Unicode")?;
    let transport = match config.transport {
        Transport::LoopbackPlaintext => PostgresTransport::LoopbackPlaintext,
        Transport::VerifyFull { additional_ca_file } => PostgresTransport::VerifyFull {
            additional_ca_pem: additional_ca_file
                .map(|file| {
                    // Relative CA paths belong to this configuration's directory.
                    read_bounded(
                        &path.parent().unwrap_or(Path::new(".")).join(file),
                        1024 * 1024,
                    )
                })
                .transpose()?,
        },
    };
    Ok(Loaded {
        selected,
        config: PostgresConfig {
            host: config.host,
            port: config.port,
            database: config.database,
            username: config.username,
            password: Some(password),
            schema,
            transport,
            limits: PostgresLimits::default(),
        },
    })
}
