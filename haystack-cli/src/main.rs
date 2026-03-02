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
    /// Import entities from a file (Zinc, Trio, JSON, or HBF format)
    Import {
        /// Path to the input file
        file: String,
        /// Input format: zinc, trio, json, json3, hbf (auto-detected from extension if omitted)
        #[arg(short, long)]
        format: Option<String>,
    },
    /// Export entities to a specified format
    Export {
        /// Output format: zinc, trio, json, json3, hbf
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
        /// TOML file with federation connector configuration
        #[arg(long)]
        federation: Option<String>,
        /// Directory for periodic snapshots (enables auto-restore on startup)
        #[arg(long)]
        snapshot_dir: Option<String>,
        /// Snapshot interval in seconds (default: 300)
        #[arg(long, default_value = "300")]
        snapshot_interval: u64,
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
    /// Create a binary snapshot of entities from an input file
    Snapshot {
        /// Directory to write the snapshot file to
        #[arg(short, long)]
        dir: String,
        /// Path to the input file (Zinc, Trio, or JSON format)
        #[arg(short, long)]
        input: Option<String>,
        /// Input format: zinc, trio, json, json3 (auto-detected from extension if omitted)
        #[arg(short, long)]
        format: Option<String>,
    },
    /// Restore entities from a binary snapshot file
    Restore {
        /// Path to the snapshot (.hlss) file
        #[arg(short, long)]
        snapshot: String,
        /// Path to export restored data to (omit to just print metadata)
        #[arg(short, long)]
        output: Option<String>,
        /// Output format: zinc, trio, json, json3
        #[arg(short, long)]
        format: Option<String>,
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
        /// Password
        #[arg(short = 'P', long)]
        password: String,
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
        /// Password
        #[arg(short = 'P', long)]
        password: String,
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
        /// Password
        #[arg(short = 'P', long)]
        password: String,
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
        /// Password
        #[arg(short = 'P', long)]
        password: String,
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
        /// Password
        #[arg(short = 'P', long)]
        password: String,
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
        /// Password
        #[arg(short = 'P', long)]
        password: String,
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
        /// Password
        #[arg(short = 'P', long)]
        password: String,
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
        /// Password for the user
        #[arg(short, long)]
        password: String,
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
        /// New password
        #[arg(short, long)]
        password: String,
    },
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
            federation,
            snapshot_dir,
            snapshot_interval,
        } => commands::serve::run(commands::serve::ServeConfig {
            port,
            file: file.as_deref(),
            users_file: users.as_deref(),
            host: host.as_deref(),
            demo,
            federation_file: federation.as_deref(),
            snapshot_dir: snapshot_dir.as_deref(),
            _snapshot_interval: snapshot_interval,
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
            } => commands::client::run_about(&url, &username, &password, &format),
            ClientAction::Read {
                url,
                username,
                password,
                filter,
                limit,
                format,
            } => commands::client::run_read(&url, &username, &password, &filter, limit, &format),
            ClientAction::Nav {
                url,
                username,
                password,
                nav_id,
                format,
            } => commands::client::run_nav(&url, &username, &password, nav_id.as_deref(), &format),
            ClientAction::HisRead {
                url,
                username,
                password,
                id,
                range,
                format,
            } => commands::client::run_his_read(&url, &username, &password, &id, &range, &format),
            ClientAction::Ops {
                url,
                username,
                password,
                format,
            } => commands::client::run_ops(&url, &username, &password, &format),
            ClientAction::Libs {
                url,
                username,
                password,
                format,
            } => commands::client::run_libs(&url, &username, &password, &format),
            ClientAction::Specs {
                url,
                username,
                password,
                lib,
                format,
            } => commands::client::run_specs(&url, &username, &password, lib.as_deref(), &format),
        },
        Commands::User { action } => match action {
            UserAction::Add {
                file,
                username,
                password,
                permissions,
            } => commands::user::run_add(&file, &username, &password, &permissions),
            UserAction::Delete { file, username } => commands::user::run_delete(&file, &username),
            UserAction::List { file } => commands::user::run_list(&file),
            UserAction::Passwd {
                file,
                username,
                password,
            } => commands::user::run_update_password(&file, &username, &password),
        },
        Commands::Snapshot { dir, input, format } => {
            commands::snapshot::run_snapshot(&dir, input.as_deref(), format.as_deref())
        }
        Commands::Restore {
            snapshot,
            output,
            format,
        } => commands::snapshot::run_restore(&snapshot, output.as_deref(), format.as_deref()),
    }
}
