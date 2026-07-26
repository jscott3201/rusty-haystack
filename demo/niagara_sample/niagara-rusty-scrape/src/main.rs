//! Read current-value Haystack points from a Niagara nHaystack station.
//!
//! Niagara nHaystack uses **HTTP Basic** auth (`HTTPBasicScheme` in Workbench), not
//! Project Haystack SCRAM. Use `--auth basic` (default) for Niagara.
//!
//! SCRAM (`--auth scram`) works against rusty-haystack server and SkySpark; on Niagara
//! it fails because nHaystack returns HTML 401 without `WWW-Authenticate: SCRAM`.

use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use clap::Parser;
use haystack_client::{AuthMode, ClientConfig, HaystackClient};

#[derive(Parser)]
#[command(
    name = "niagara-read",
    about = "Haystack point read (Niagara nHaystack demo)"
)]
struct Args {
    /// Haystack API root (example: https://<jace-host>/haystack)
    #[arg(long, env = "HAYSTACK_BASE")]
    url: String,

    #[arg(long, env = "HAYSTACK_USER")]
    user: String,

    #[arg(long, env = "HAYSTACK_PASS")]
    password: String,

    /// Auth mode: basic (Niagara) or scram (SkySpark / rusty-haystack server)
    #[arg(long, env = "HAYSTACK_AUTH", default_value = "basic")]
    auth: AuthChoice,

    /// Verify TLS certificates (secure default). Use --insecure-tls for self-signed lab certs.
    #[arg(long, env = "HAYSTACK_TLS_VERIFY", default_value_t = true)]
    tls_verify: bool,

    /// Disable TLS certificate and hostname verification (lab self-signed certs only)
    #[arg(long)]
    insecure_tls: bool,

    /// Permit Basic auth against an http:// URL. The client refuses this by
    /// default: Basic sends a reusable password on every request, so over plain
    /// HTTP it is disclosed to anyone on the path.
    #[arg(long)]
    allow_plaintext_basic: bool,

    #[arg(long, default_value = "point and cur")]
    filter: String,

    #[arg(long, default_value_t = 20)]
    limit: usize,

    /// Probe SCRAM HELLO even when auth=basic (shows why Niagara rejects SCRAM)
    #[arg(long)]
    probe_scram: bool,
}

#[derive(Debug, Clone, clap::ValueEnum)]
enum AuthChoice {
    Basic,
    Scram,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    haystack_client::ensure_crypto_provider();

    let auth_mode = match args.auth {
        AuthChoice::Basic => AuthMode::Basic,
        AuthChoice::Scram => AuthMode::Scram,
    };

    let tls_verify = if args.insecure_tls {
        false
    } else {
        args.tls_verify
    };

    if !tls_verify {
        eprintln!(
            "WARNING: TLS certificate AND hostname verification disabled — \
             credentials may be exposed to network attackers (lab/dev only)"
        );
    }

    let config = ClientConfig {
        tls_verify,
        auth_mode,
        allow_plaintext_basic: args.allow_plaintext_basic,
        ..ClientConfig::default()
    };

    println!("url: {}", args.url.trim_end_matches('/'));
    println!("user: {}", args.user);
    println!("auth: {:?}", auth_mode);
    println!("tls_verify: {tls_verify}");
    println!();

    if let Err(hint) = preflight_tcp(&args.url) {
        eprintln!("PREFLIGHT FAIL: {hint}");
        std::process::exit(1);
    }
    println!("preflight: TCP endpoint reachable");
    println!();

    if args.probe_scram || auth_mode == AuthMode::Scram {
        probe_scram_hello(&args, tls_verify).await;
    }

    match HaystackClient::connect_with_config(
        args.url.trim_end_matches('/'),
        &args.user,
        &args.password,
        &config,
    )
    .await
    {
        Ok(client) => match client.read(&args.filter, Some(args.limit)).await {
            Ok(grid) => {
                println!("read filter={:?} → {} rows", args.filter, grid.rows.len());
                for row in &grid.rows {
                    let dis = kind_display(row.get("dis").or_else(|| row.get("id")));
                    let cur = kind_display(row.get("curVal"));
                    let unit = kind_display(row.get("unit"));
                    let slot =
                        kind_display(row.get("n4SlotPath").or_else(|| row.get("axSlotPath")));
                    if dis.contains("BacnetNetwork") || slot.contains("BacnetNetwork") {
                        println!("  {dis}  {cur} {unit}");
                    }
                }
            }
            Err(err) => {
                eprintln!("read failed: {err}");
                std::process::exit(1);
            }
        },
        Err(err) => {
            eprintln!("connect failed: {err}");
            print_connect_hint(&args.url, &err.to_string());
            if auth_mode == AuthMode::Scram {
                eprintln!();
                eprintln!("Niagara nHaystack does not implement Haystack SCRAM on /about.");
                eprintln!(
                    "Use: --auth basic   (and HTTPBasicScheme for the service user in Workbench)"
                );
            }
            std::process::exit(1);
        }
    }
}

async fn probe_scram_hello(args: &Args, tls_verify: bool) {
    use base64::engine::general_purpose::STANDARD as BASE64;
    use base64::Engine;
    use haystack_core::auth;

    println!("--- SCRAM HELLO probe ---");
    let config = ClientConfig {
        tls_verify,
        auth_mode: AuthMode::Scram,
        ..ClientConfig::default()
    };
    let Ok(client) = config.build_reqwest_client() else {
        println!("could not build HTTP client");
        return;
    };
    let url = format!("{}/about", args.url.trim_end_matches('/'));
    let username_b64 = BASE64.encode(args.user.as_bytes());
    let (_, client_first_b64) = auth::client_first_message(&args.user);
    let hello = format!("HELLO username={username_b64}, data={client_first_b64}");
    match client.get(&url).header("Authorization", hello).send().await {
        Ok(resp) => {
            let www = resp
                .headers()
                .get("www-authenticate")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("<missing>");
            println!("status: {}", resp.status());
            println!("www-authenticate: {www}");
        }
        Err(err) => println!("transport error: {err}"),
    }
    println!();
}

fn tcp_endpoint(base: &str) -> Option<(String, u16)> {
    let rest = base.trim_end_matches('/');
    let (scheme_host, default_port) = match rest.strip_prefix("https://") {
        Some(s) => (s, 443u16),
        None => (rest.strip_prefix("http://")?, 80u16),
    };
    let host_port = scheme_host.split('/').next()?;
    if let Some((host, port)) = host_port.split_once(':') {
        port.parse().ok().map(|p| (host.to_string(), p))
    } else {
        Some((host_port.to_string(), default_port))
    }
}

fn preflight_tcp(base: &str) -> Result<(), String> {
    let (host, port) =
        tcp_endpoint(base).ok_or_else(|| format!("could not parse host/port from url: {base}"))?;
    if host.contains('<') || host.contains('>') {
        return Err(format!(
            "url host looks like a placeholder ({host}) — edit .env (see env.example)"
        ));
    }

    let timeout = Duration::from_secs(5);
    let addrs: Vec<SocketAddr> = format!("{host}:{port}")
        .to_socket_addrs()
        .map_err(|e| format!("DNS/parse error for {host}:{port}: {e}"))?
        .collect();

    for addr in addrs {
        if TcpStream::connect_timeout(&addr, timeout).is_ok() {
            return Ok(());
        }
    }

    let mut hint = format!("cannot open TCP {host}:{port} from this host within {timeout:?}");
    if host == "192.168.204.11" && port == 443 {
        hint.push_str(
            "\n\nNiagara bench fix (Windows PC at 192.168.204.11):\n\
             1. Station running + nHaystack servlet enabled\n\
             2. Windows Firewall → allow inbound TCP 443 from 192.168.204.55\n\
             3. Re-test:  curl -k -m 5 -u open_fdd:PASS https://192.168.204.11/haystack/about\n\
             (ICMP ping may fail even when HTTPS works — that is normal on Windows)",
        );
    }
    Err(hint)
}

fn print_connect_hint(base: &str, err: &str) {
    if err.contains("timed out") || err.contains("connect") {
        if let Some((host, port)) = tcp_endpoint(base) {
            eprintln!();
            eprintln!("Hint: TCP {host}:{port} may be blocked (firewall) or the station is down.");
            if host == "192.168.204.11" {
                eprintln!(
                    "Open-FDD bench: allow TCP 443 on the Niagara Windows host for 192.168.204.55."
                );
            }
        }
    }
}

fn kind_display(kind: Option<&haystack_core::kinds::Kind>) -> String {
    match kind {
        Some(k) => format!("{k}"),
        None => "-".to_string(),
    }
}
