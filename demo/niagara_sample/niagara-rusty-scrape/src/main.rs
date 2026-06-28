//! Read current-value Haystack points from a Niagara nHaystack station.
//!
//! Niagara nHaystack uses **HTTP Basic** auth (`HTTPBasicScheme` in Workbench), not
//! Project Haystack SCRAM. Use `--auth basic` (default) for Niagara.
//!
//! SCRAM (`--auth scram`) works against rusty-haystack server and SkySpark; on Niagara
//! it fails because nHaystack returns HTML 401 without `WWW-Authenticate: SCRAM`.

use clap::Parser;
use haystack_client::{AuthMode, ClientConfig, HaystackClient};

#[derive(Parser, Debug)]
#[command(name = "niagara-read", about = "Haystack point read (Niagara nHaystack demo)")]
struct Args {
    /// Haystack API root, e.g. https://192.168.204.11/haystack
    #[arg(long, env = "HAYSTACK_BASE", default_value = "https://192.168.204.11/haystack")]
    url: String,

    #[arg(long, env = "HAYSTACK_USER", default_value = "open_fdd")]
    user: String,

    #[arg(long, env = "HAYSTACK_PASS")]
    password: String,

    /// Auth mode: basic (Niagara) or scram (SkySpark / rusty-haystack server)
    #[arg(long, env = "HAYSTACK_AUTH", default_value = "basic")]
    auth: AuthChoice,

    /// Verify TLS certificates (off by default for Niagara self-signed lab cert)
    #[arg(long, env = "HAYSTACK_TLS_VERIFY", default_value_t = false)]
    tls_verify: bool,

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

    let config = ClientConfig {
        tls_verify: args.tls_verify,
        auth_mode,
        ..ClientConfig::default()
    };

    println!("url: {}", args.url.trim_end_matches('/'));
    println!("user: {}", args.user);
    println!("auth: {:?}", auth_mode);
    println!("tls_verify: {}", args.tls_verify);
    println!();

    if args.probe_scram || auth_mode == AuthMode::Scram {
        probe_scram_hello(&args).await;
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
                    let slot = kind_display(
                        row.get("n4SlotPath").or_else(|| row.get("axSlotPath")),
                    );
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
            if auth_mode == AuthMode::Scram {
                eprintln!();
                eprintln!("Niagara nHaystack does not implement Haystack SCRAM on /about.");
                eprintln!("Use: --auth basic   (and HTTPBasicScheme for the service user in Workbench)");
            }
            std::process::exit(1);
        }
    }
}

async fn probe_scram_hello(args: &Args) {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use haystack_core::auth;

    println!("--- SCRAM HELLO probe ---");
    let config = ClientConfig::scram_insecure_tls();
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

fn kind_display(kind: Option<&haystack_core::kinds::Kind>) -> String {
    match kind {
        Some(k) => format!("{k}"),
        None => "-".to_string(),
    }
}
