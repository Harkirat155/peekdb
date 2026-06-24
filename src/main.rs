mod db;
mod server;

use clap::Parser;
use db::postgres::PostgresDb;
use db::traits::Database;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "mcp-db-readonly",
    about = "MCP server for read-only database access.\nAll output (logs) goes to stderr; stdout carries the MCP protocol.",
    version
)]
struct Args {
    /// PostgreSQL connection URL. Example: postgres://user:pass@localhost:5432/mydb
    #[arg(long, env = "DATABASE_URL")]
    database_url: String,

    /// Default maximum rows returned per query (user can override up to 5000)
    #[arg(long, default_value = "500", env = "MCP_MAX_ROWS")]
    max_rows: usize,

    /// Disable TLS — use for local/dev connections without SSL
    #[arg(long, default_value_t = false)]
    no_tls: bool,
}

#[tokio::main]
async fn main() {
    // Initialize tracing to stderr so it never pollutes MCP stdout
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("RUST_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();

    let db = match PostgresDb::connect(&args.database_url, args.no_tls).await {
        Ok(db) => Arc::new(db) as Arc<dyn Database>,
        Err(e) => {
            eprintln!("Fatal: {}", e);
            std::process::exit(1);
        }
    };

    server::run(db, args.max_rows).await;
}
