use super::PostgresError;
use sqlx::{
    ConnectOptions,
    postgres::{PgConnectOptions, PgSslMode},
};
use std::{fmt, net::IpAddr, time::Duration};

/// Explicit transport policy. Verified TLS uses the bundled WebPKI roots plus
/// this optional CA. The additional CA is not an exclusive trust pin.
#[derive(Clone)]
pub enum PostgresTransport {
    VerifyFull {
        additional_ca_pem: Option<Vec<u8>>,
    },
    /// Deliberate local development/test opt-in; accepts literal loopback IPs
    /// and `localhost` only. There is no opportunistic encryption fallback.
    LoopbackPlaintext,
}
impl fmt::Debug for PostgresTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::VerifyFull { additional_ca_pem } => f
                .debug_struct("VerifyFull")
                .field("additional_ca", &additional_ca_pem.is_some())
                .finish(),
            Self::LoopbackPlaintext => f.write_str("LoopbackPlaintext"),
        }
    }
}

/// Finite driver and recovery limits. Every connection uses these fixed server
/// deadlines in addition to the caller's absolute application deadline.
#[derive(Debug, Clone)]
pub struct PostgresLimits {
    pub pool_connections: u32,
    pub acquire_timeout: Duration,
    pub statement_timeout: Duration,
    pub lock_timeout: Duration,
    pub transaction_timeout: Duration,
    pub snapshot_timeout: Duration,
    /// Maximum idle delay before beginning a bounded reconciliation attempt.
    pub refresh_interval: Duration,
    pub recovery: haystack_core::graph::RecoveryLimits,
    pub max_receipts: usize,
    pub max_receipt_bytes: usize,
    pub max_history_samples: usize,
}
impl Default for PostgresLimits {
    fn default() -> Self {
        Self {
            pool_connections: 4,
            acquire_timeout: Duration::from_secs(5),
            statement_timeout: Duration::from_secs(5),
            lock_timeout: Duration::from_secs(3),
            transaction_timeout: Duration::from_secs(15),
            snapshot_timeout: Duration::from_secs(10),
            refresh_interval: Duration::from_millis(250),
            recovery: Default::default(),
            max_receipts: 1024,
            max_receipt_bytes: 64 * 1024 * 1024,
            max_history_samples: 100_000,
        }
    }
}

/// Structured connection configuration. The process environment must remain
/// immutable while options are constructed. Conflicting PG environment keys
/// are rejected, including empty/non-Unicode values; passfiles are never read.
#[derive(Clone)]
pub struct PostgresConfig {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    pub password: Option<String>,
    /// A task/deployment-owned schema, restricted to ASCII identifiers.
    pub schema: String,
    pub transport: PostgresTransport,
    pub limits: PostgresLimits,
}
impl fmt::Debug for PostgresConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PostgresConfig")
            .field("connection", &"[redacted]")
            .field("transport", &self.transport)
            .field("limits", &self.limits)
            .finish()
    }
}
const PG_ENV: [&str; 12] = [
    "PGPORT",
    "PGHOSTADDR",
    "PGHOST",
    "PGUSER",
    "PGDATABASE",
    "PGPASSWORD",
    "PGSSLROOTCERT",
    "PGSSLCERT",
    "PGSSLKEY",
    "PGSSLMODE",
    "PGAPPNAME",
    "PGOPTIONS",
];
impl PostgresConfig {
    pub(crate) fn options(&self) -> Result<PgConnectOptions, PostgresError> {
        if PG_ENV.iter().any(|key| std::env::var_os(key).is_some()) {
            return Err(PostgresError::AmbientConfiguration);
        }
        self.validate()?;
        let mut options = PgConnectOptions::new_without_pgpass()
            .host(&self.host)
            .port(self.port)
            .username(&self.username)
            .database(&self.database)
            .application_name(&format!("rusty-haystack:{}", self.schema))
            .disable_statement_logging();
        if let Some(password) = &self.password {
            options = options.password(password);
        }
        options = match &self.transport {
            PostgresTransport::VerifyFull { additional_ca_pem } => {
                let options = options.ssl_mode(PgSslMode::VerifyFull);
                if let Some(pem) = additional_ca_pem {
                    options.ssl_root_cert_from_pem(pem.clone())
                } else {
                    options
                }
            }
            PostgresTransport::LoopbackPlaintext => options.ssl_mode(PgSslMode::Disable),
        };
        Ok(options.options([
            (
                "statement_timeout",
                self.limits.statement_timeout.as_millis().to_string(),
            ),
            (
                "lock_timeout",
                self.limits.lock_timeout.as_millis().to_string(),
            ),
            (
                "transaction_timeout",
                self.limits.transaction_timeout.as_millis().to_string(),
            ),
            (
                "idle_in_transaction_session_timeout",
                self.limits.transaction_timeout.as_millis().to_string(),
            ),
            ("synchronous_commit", "on".to_owned()),
            ("search_path", format!("\"{}\",pg_catalog", self.schema)),
        ]))
    }
    fn validate(&self) -> Result<(), PostgresError> {
        let bounded = |s: &str| !s.is_empty() && s.len() <= 256 && !s.contains('\0');
        if !bounded(&self.host)
            || self.host.contains('/')
            || self.port == 0
            || !bounded(&self.database)
            || !bounded(&self.username)
            || self
                .password
                .as_ref()
                .is_some_and(|p| p.len() > 8192 || p.contains('\0'))
            || self.schema.is_empty()
            || self.schema.len() > 48
            || !self
                .schema
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            || !self.schema.as_bytes()[0].is_ascii_lowercase()
            || self.schema == "public"
            || self.schema.starts_with("pg_")
        {
            return Err(PostgresError::Configuration);
        }
        if matches!(self.transport, PostgresTransport::LoopbackPlaintext)
            && self.host != "localhost"
            && !self.host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
        {
            return Err(PostgresError::Configuration);
        }
        if let PostgresTransport::VerifyFull {
            additional_ca_pem: Some(pem),
        } = &self.transport
            && (pem.is_empty() || pem.len() > 1024 * 1024)
        {
            return Err(PostgresError::Configuration);
        }
        let l = &self.limits;
        if l.max_receipts == 0
            || l.max_receipts > 100_000
            || l.max_receipt_bytes == 0
            || l.max_receipt_bytes > 256 * 1024 * 1024
            || l.max_history_samples == 0
            || l.max_history_samples > 1_000_000
            || l.pool_connections == 0
            || l.pool_connections > 32
            || [
                l.acquire_timeout,
                l.statement_timeout,
                l.lock_timeout,
                l.transaction_timeout,
                l.snapshot_timeout,
                l.refresh_interval,
            ]
            .iter()
            .any(|d| *d < Duration::from_millis(1) || *d > Duration::from_secs(60))
            || l.snapshot_timeout >= l.transaction_timeout
        {
            return Err(PostgresError::Configuration);
        }
        l.recovery
            .validate()
            .map_err(|_| PostgresError::Configuration)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> PostgresConfig {
        PostgresConfig {
            host: "localhost".into(),
            port: 5432,
            database: "db".into(),
            username: "user".into(),
            password: Some("secret-sentinel".into()),
            schema: "test_storage".into(),
            transport: PostgresTransport::LoopbackPlaintext,
            limits: PostgresLimits::default(),
        }
    }
    #[test]
    fn configuration_errors_and_debug_redact_secrets() {
        let mut c = config();
        assert!(!format!("{c:?}").contains("secret-sentinel"));
        c.host = "https://secret-sentinel".into();
        assert_eq!(
            c.validate().unwrap_err().to_string(),
            "invalid PostgreSQL configuration"
        );
        c.host = "example.com".into();
        assert!(c.validate().is_err());
        c.host = "127.0.0.1".into();
        c.schema = "x; DROP TABLE y".into();
        assert!(c.validate().is_err());
        c.schema = "test_storage".into();
        assert!(c.validate().is_ok());
        c.transport = PostgresTransport::VerifyFull {
            additional_ca_pem: Some(b"certificate-sentinel".to_vec()),
        };
        assert!(!format!("{c:?}").contains("certificate-sentinel"));
    }
    #[test]
    fn ambient_configuration_is_rejected_in_isolated_process() {
        const CHILD: &str = "RUSTY_HAYSTACK_PG_CONFIG_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            assert!(matches!(
                config().options(),
                Err(PostgresError::AmbientConfiguration)
            ));
            return;
        }
        let values = vec![
            std::ffi::OsString::from(""),
            std::ffi::OsString::from("ambient-sentinel"),
        ];
        #[cfg(unix)]
        let values = {
            use std::os::unix::ffi::OsStringExt;
            let mut values = values;
            values.push(std::ffi::OsString::from_vec(vec![0xff]));
            values
        };
        for key in PG_ENV {
            for value in &values {
                let mut child = std::process::Command::new(std::env::current_exe().unwrap());
                for key in PG_ENV {
                    child.env_remove(key);
                }
                let output = child.args(["--exact", "postgres::config::tests::ambient_configuration_is_rejected_in_isolated_process"])
                .env(CHILD, "1").env(key, value).output().unwrap();
                assert!(
                    output.status.success(),
                    "ambient key {key} was not rejected"
                );
            }
        }
    }
}
