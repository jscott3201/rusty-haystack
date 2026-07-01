mod commands;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "haystack", version, about = "Project Haystack CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Import entities from a file (Zinc, Trio, JSON format)
    Import {
        /// Path to the input file
        file: String,
        /// Input format: zinc, trio, json, json3 (auto-detected from extension if omitted)
        #[arg(short, long)]
        format: Option<String>,
    },
    /// Export entities to a specified format
    Export {
        /// Output format: zinc, trio, json, json3
        #[arg(short, long, default_value = "zinc")]
        format: String,
        /// Path to export to (stdout if omitted)
        #[arg(short, long)]
        output: Option<String>,
        /// Filter expression to select entities
        #[arg(long)]
        filter: Option<String>,
    },
    /// Start the Haystack HTTP API server
    Serve {
        /// Port to listen on
        #[arg(short, long, default_value = "8080")]
        port: u16,
        /// File to load entities from at startup
        #[arg(short, long)]
        file: Option<String>,
        /// TOML file with user credentials for SCRAM authentication
        #[arg(short, long)]
        users: Option<String>,
        /// Host to bind to
        #[arg(long)]
        host: Option<String>,
        /// Load a demo building automation dataset
        #[arg(long)]
        demo: bool,
    },
    /// Validate entities in a file against the standard Haystack ontology
    Validate {
        /// Path to the input file
        file: String,
        /// Input format (auto-detected from extension if omitted)
        #[arg(short, long)]
        format: Option<String>,
        /// Path to a directory containing custom .xeto library files (repeatable)
        #[arg(long = "xeto-dir", value_name = "DIR")]
        xeto_dirs: Vec<String>,
        /// Output full structured validation report instead of just summary
        #[arg(long)]
        report: bool,
    },
    /// Show information about the Haystack standard library
    Info {
        /// Show info about a specific def (e.g., "ahu", "site", "equip")
        #[arg(short, long)]
        def: Option<String>,
    },
    /// List loaded libraries in the standard namespace
    Libs,
    /// List loaded Xeto specs
    Specs {
        /// Filter specs by library name
        #[arg(short, long)]
        lib: Option<String>,
    },
    /// Query a remote Haystack server
    Client {
        #[command(subcommand)]
        action: ClientAction,
    },
    /// Manage users in a TOML credentials file
    User {
        #[command(subcommand)]
        action: UserAction,
    },
}

#[derive(Subcommand)]
enum ClientAction {
    /// Get server information
    About {
        /// Server API URL (e.g., http://localhost:8080/api)
        #[arg(short, long)]
        url: String,
        /// Username
        #[arg(short = 'U', long)]
        username: String,
        /// Password (or set HAYSTACK_PASSWORD env var)
        #[arg(short = 'P', long)]
        password: Option<String>,
        /// Output format: zinc, json, trio, json3
        #[arg(short, long, default_value = "zinc")]
        format: String,
    },
    /// Read entities by filter
    Read {
        /// Server API URL
        #[arg(short, long)]
        url: String,
        /// Username
        #[arg(short = 'U', long)]
        username: String,
        /// Password (or set HAYSTACK_PASSWORD env var)
        #[arg(short = 'P', long)]
        password: Option<String>,
        /// Filter expression
        filter: String,
        /// Maximum number of rows to return
        #[arg(short, long)]
        limit: Option<usize>,
        /// Output format: zinc, json, trio, json3
        #[arg(short, long, default_value = "zinc")]
        format: String,
    },
    /// Navigate entity tree
    Nav {
        /// Server API URL
        #[arg(short, long)]
        url: String,
        /// Username
        #[arg(short = 'U', long)]
        username: String,
        /// Password (or set HAYSTACK_PASSWORD env var)
        #[arg(short = 'P', long)]
        password: Option<String>,
        /// Navigation ID (omit for root)
        #[arg(long)]
        nav_id: Option<String>,
        /// Output format: zinc, json, trio, json3
        #[arg(short, long, default_value = "zinc")]
        format: String,
    },
    /// Read historical data for a point
    HisRead {
        /// Server API URL
        #[arg(short, long)]
        url: String,
        /// Username
        #[arg(short = 'U', long)]
        username: String,
        /// Password (or set HAYSTACK_PASSWORD env var)
        #[arg(short = 'P', long)]
        password: Option<String>,
        /// Point entity ID
        id: String,
        /// Date range (e.g., "today", "yesterday", "2024-01-01,2024-01-31")
        #[arg(short, long)]
        range: String,
        /// Output format: zinc, json, trio, json3
        #[arg(short, long, default_value = "zinc")]
        format: String,
    },
    /// List supported server operations
    Ops {
        /// Server API URL
        #[arg(short, long)]
        url: String,
        /// Username
        #[arg(short = 'U', long)]
        username: String,
        /// Password (or set HAYSTACK_PASSWORD env var)
        #[arg(short = 'P', long)]
        password: Option<String>,
        /// Output format: zinc, json, trio, json3
        #[arg(short, long, default_value = "zinc")]
        format: String,
    },
    /// List libraries from a remote server
    Libs {
        /// Server API URL
        #[arg(short, long)]
        url: String,
        /// Username
        #[arg(short = 'U', long)]
        username: String,
        /// Password (or set HAYSTACK_PASSWORD env var)
        #[arg(short = 'P', long)]
        password: Option<String>,
        /// Output format: zinc, json, trio, json3
        #[arg(short, long, default_value = "zinc")]
        format: String,
    },
    /// List specs from a remote server
    Specs {
        /// Server API URL
        #[arg(short, long)]
        url: String,
        /// Username
        #[arg(short = 'U', long)]
        username: String,
        /// Password (or set HAYSTACK_PASSWORD env var)
        #[arg(short = 'P', long)]
        password: Option<String>,
        /// Filter by library name
        #[arg(short, long)]
        lib: Option<String>,
        /// Output format: zinc, json, trio, json3
        #[arg(short, long, default_value = "zinc")]
        format: String,
    },
}

#[derive(Subcommand)]
enum UserAction {
    /// Add a new user
    Add {
        /// Path to the users TOML file
        #[arg(short, long)]
        file: String,
        /// Username to add
        username: String,
        /// Password for the user (or set HAYSTACK_PASSWORD env var)
        #[arg(short, long)]
        password: Option<String>,
        /// Permissions (read, write, admin)
        #[arg(short = 'r', long, value_delimiter = ',', default_value = "read")]
        permissions: Vec<String>,
    },
    /// Delete a user
    Delete {
        /// Path to the users TOML file
        #[arg(short, long)]
        file: String,
        /// Username to delete
        username: String,
    },
    /// List all users
    List {
        /// Path to the users TOML file
        #[arg(short, long)]
        file: String,
    },
    /// Update a user's password
    Passwd {
        /// Path to the users TOML file
        #[arg(short, long)]
        file: String,
        /// Username to update
        username: String,
        /// New password (or set HAYSTACK_PASSWORD env var)
        #[arg(short, long)]
        password: Option<String>,
    },
}

fn resolve_password(password: Option<String>) -> String {
    password
        .or_else(|| std::env::var("HAYSTACK_PASSWORD").ok())
        .unwrap_or_else(|| {
            eprintln!("Error: password required (--password or HAYSTACK_PASSWORD env var)");
            std::process::exit(1);
        })
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Import { file, format } => commands::import::run(&file, format.as_deref()),
        Commands::Export {
            format,
            output,
            filter,
        } => commands::export::run(&format, output.as_deref(), filter.as_deref()),
        Commands::Serve {
            port,
            file,
            users,
            host,
            demo,
        } => commands::serve::run(commands::serve::ServeConfig {
            port,
            file: file.as_deref(),
            users_file: users.as_deref(),
            host: host.as_deref(),
            demo,
        }),
        Commands::Validate {
            file,
            format,
            xeto_dirs,
            report,
        } => commands::validate::run(&file, format.as_deref(), &xeto_dirs, report),
        Commands::Info { def } => commands::info::run(def.as_deref()),
        Commands::Libs => commands::libs::run(),
        Commands::Specs { lib } => commands::specs::run(lib.as_deref()),
        Commands::Client { action } => match action {
            ClientAction::About {
                url,
                username,
                password,
                format,
            } => {
                let password = resolve_password(password);
                commands::client::run_about(&url, &username, &password, &format);
            }
            ClientAction::Read {
                url,
                username,
                password,
                filter,
                limit,
                format,
            } => {
                let password = resolve_password(password);
                commands::client::run_read(&url, &username, &password, &filter, limit, &format);
            }
            ClientAction::Nav {
                url,
                username,
                password,
                nav_id,
                format,
            } => {
                let password = resolve_password(password);
                commands::client::run_nav(&url, &username, &password, nav_id.as_deref(), &format);
            }
            ClientAction::HisRead {
                url,
                username,
                password,
                id,
                range,
                format,
            } => {
                let password = resolve_password(password);
                commands::client::run_his_read(&url, &username, &password, &id, &range, &format);
            }
            ClientAction::Ops {
                url,
                username,
                password,
                format,
            } => {
                let password = resolve_password(password);
                commands::client::run_ops(&url, &username, &password, &format);
            }
            ClientAction::Libs {
                url,
                username,
                password,
                format,
            } => {
                let password = resolve_password(password);
                commands::client::run_libs(&url, &username, &password, &format);
            }
            ClientAction::Specs {
                url,
                username,
                password,
                lib,
                format,
            } => {
                let password = resolve_password(password);
                commands::client::run_specs(&url, &username, &password, lib.as_deref(), &format);
            }
        },
        Commands::User { action } => match action {
            UserAction::Add {
                file,
                username,
                password,
                permissions,
            } => {
                let password = resolve_password(password);
                commands::user::run_add(&file, &username, &password, &permissions);
            }
            UserAction::Delete { file, username } => commands::user::run_delete(&file, &username),
            UserAction::List { file } => commands::user::run_list(&file),
            UserAction::Passwd {
                file,
                username,
                password,
            } => {
                let password = resolve_password(password);
                commands::user::run_update_password(&file, &username, &password);
            }
        },
    }
}
