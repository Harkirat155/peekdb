mod config;
mod db;
mod server;

use clap::Parser;
use config::{interpolate_env, Config, DatabaseConfig};
use db::postgres::PostgresDb;
use db::traits::Database;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "mcp-db-readonly",
    about = "MCP server for read-only database access. Supports one or many named databases.\nLogs go to stderr; stdout carries the MCP protocol.",
    version
)]
struct Args {
    /// Path to a TOML config file listing named databases (see databases.toml.example)
    #[arg(long)]
    config: Option<PathBuf>,

    /// Single PostgreSQL connection URL (alternative to --config)
    #[arg(long, env = "DATABASE_URL")]
    database_url: Option<String>,

    /// Name for the --database-url connection
    #[arg(long, default_value = "default")]
    database_name: String,

    /// Disable TLS for the --database-url connection (use for local dev)
    #[arg(long, default_value_t = false)]
    no_tls: bool,

    /// Default maximum rows returned per query (user can override up to 5000)
    #[arg(long, default_value = "500", env = "MCP_MAX_ROWS")]
    max_rows: usize,
}

#[tokio::main]
async fn main() {
    // All logs to stderr — stdout is the MCP wire
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("RUST_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();

    let db_configs: Vec<DatabaseConfig> = match (&args.config, &args.database_url) {
        (Some(path), None) => {
            match Config::from_file(path) {
                Ok(c) => c.database,
                Err(e) => { eprintln!("Fatal: {}", e); std::process::exit(1); }
            }
        }
        (None, Some(url)) => vec![DatabaseConfig {
            name: args.database_name.clone(),
            url: url.clone(),
            no_tls: args.no_tls,
        }],
        (Some(_), Some(_)) => {
            eprintln!("Fatal: --config and --database-url are mutually exclusive");
            std::process::exit(1);
        }
        (None, None) => {
            eprintln!("Fatal: provide --config <path> or --database-url <url>");
            std::process::exit(1);
        }
    };

    // Resolve env-var placeholders in each URL, then create lazy DB handles
    let mut databases: HashMap<String, Arc<dyn Database>> = HashMap::new();
    for cfg in db_configs {
        let resolved_url = match interpolate_env(&cfg.url) {
            Ok(u) => u,
            Err(e) => {
                eprintln!("Fatal: database '{}': {}", cfg.name, e);
                std::process::exit(1);
            }
        };
        let db = PostgresDb::new(&cfg.name, resolved_url, cfg.no_tls);
        databases.insert(cfg.name, Arc::new(db) as Arc<dyn Database>);
    }

    server::run(databases, args.max_rows).await;
}
