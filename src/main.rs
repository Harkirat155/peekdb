mod cli;
mod config;
mod db;
mod server;

use clap::{Parser, Subcommand};
use config::{interpolate_env, Config, DatabaseConfig};
use db::mongo::MongoDb;
use db::postgres::PostgresDb;
use db::traits::{Database, MongoDatabase, MongoOperation, MongoQuery};
use mongodb::options::ClientOptions;
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

// ── Top-level CLI ─────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(
    name = "mcp-db-readonly",
    about = "Read-only database access — MCP server (serve) or direct CLI queries (query / mongo).\n\
             Running with --config without a subcommand defaults to 'serve' for backwards compatibility.",
    version
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run as an MCP server on stdio (default when called with --config / --database-url)
    Serve(ServeArgs),
    /// Execute a PostgreSQL query and print results
    Query(QueryArgs),
    /// Execute a MongoDB query and print results
    Mongo(MongoArgs),
}

// ── Serve sub-command (original behaviour) ────────────────────────────────────

#[derive(clap::Args, Debug)]
struct ServeArgs {
    /// Path to a TOML config file listing named databases
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

    /// Default maximum rows returned per query
    #[arg(long, default_value = "500", env = "MCP_MAX_ROWS")]
    max_rows: usize,
}

// ── Query sub-command (Postgres CLI) ─────────────────────────────────────────

#[derive(clap::Args, Debug)]
struct QueryArgs {
    /// Path to databases.toml
    #[arg(long)]
    config: PathBuf,

    /// Database name from config (can omit if only one Postgres DB is configured)
    #[arg(long)]
    db: Option<String>,

    /// SQL to execute. Reads from stdin if omitted (pipe or heredoc)
    #[arg(long)]
    sql: Option<String>,

    /// Max rows to return
    #[arg(long, default_value = "500")]
    max_rows: usize,

    /// Output rows as a JSON array instead of a table
    #[arg(long)]
    json: bool,
}

// ── Mongo sub-command ─────────────────────────────────────────────────────────

#[derive(clap::Args, Debug)]
struct MongoArgs {
    /// Path to databases.toml
    #[arg(long)]
    config: PathBuf,

    /// MongoDB database name from config (can omit if only one Mongo DB is configured)
    #[arg(long)]
    db: Option<String>,

    /// Collection name (required for most operations)
    #[arg(long)]
    collection: Option<String>,

    /// Operation: find | aggregate | distinct | count | list_collections | list_indexes
    #[arg(long, default_value = "find")]
    operation: String,

    /// Filter as a JSON object, e.g. '{"status":"IN_PROGRESS"}'
    #[arg(long)]
    filter: Option<String>,

    /// Pipeline as a JSON array (for aggregate)
    #[arg(long)]
    pipeline: Option<String>,

    /// Projection as a JSON object (for find)
    #[arg(long)]
    projection: Option<String>,

    /// Sort as a JSON object, e.g. '{"fire_time":-1}' (for find)
    #[arg(long)]
    sort: Option<String>,

    /// Field name for distinct
    #[arg(long)]
    field: Option<String>,

    /// Max documents to return
    #[arg(long, default_value = "100")]
    limit: usize,

    /// Output as a JSON array instead of pretty-printed documents
    #[arg(long)]
    json: bool,
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("RUST_LOG").unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    // Inject "serve" as the default sub-command so that legacy invocations like
    //   mcp-db-readonly --config databases.toml
    // continue to work without modification.
    let raw: Vec<String> = std::env::args().collect();
    let args = Args::parse_from(inject_serve_default(raw));

    match args.command {
        Command::Serve(a) => run_serve(a).await,
        Command::Query(a) => run_query(a).await,
        Command::Mongo(a) => run_mongo(a).await,
    }
}

/// If the first positional argument is not a known sub-command name (or a help/
/// version flag), prepend "serve" so that bare `--config ...` still works.
fn inject_serve_default(mut args: Vec<String>) -> Vec<String> {
    const SUBCOMMANDS: &[&str] = &["serve", "query", "mongo"];
    const PASSTHROUGH: &[&str] = &["--help", "-h", "--version", "-V"];

    if let Some(first) = args.get(1).map(|s| s.as_str()) {
        if !SUBCOMMANDS.contains(&first) && !PASSTHROUGH.contains(&first) {
            args.insert(1, "serve".to_string());
        }
    }
    args
}

// ── Shared database setup helpers ─────────────────────────────────────────────

async fn build_pg_databases(path: &PathBuf) -> HashMap<String, Arc<dyn Database>> {
    let config = load_config(path);
    let mut databases: HashMap<String, Arc<dyn Database>> = HashMap::new();
    for cfg in config.database {
        let url = resolve_url(&cfg.name, &cfg.url);
        databases.insert(cfg.name.clone(), Arc::new(PostgresDb::new(&cfg.name, url, cfg.no_tls)));
    }
    databases
}

async fn build_mongo_databases(path: &PathBuf) -> HashMap<String, Arc<dyn MongoDatabase>> {
    let config = load_config(path);
    let mut dbs: HashMap<String, Arc<dyn MongoDatabase>> = HashMap::new();
    for cfg in config.mongo {
        let uri = resolve_url(&cfg.name, &cfg.uri);
        let opts = ClientOptions::parse(&uri).await.unwrap_or_else(|e| {
            eprintln!("Fatal: mongo '{}' URI parse: {}", cfg.name, e);
            std::process::exit(1);
        });
        let dbname = opts.default_database.unwrap_or_else(|| {
            eprintln!("Fatal: mongo '{}' URI must include the database name", cfg.name);
            std::process::exit(1);
        });
        dbs.insert(cfg.name.clone(), Arc::new(MongoDb::new(&cfg.name, uri, dbname)));
    }
    dbs
}

fn load_config(path: &PathBuf) -> Config {
    Config::from_file(path).unwrap_or_else(|e| {
        eprintln!("Fatal: {}", e);
        std::process::exit(1);
    })
}

fn resolve_url(name: &str, raw: &str) -> String {
    interpolate_env(raw).unwrap_or_else(|e| {
        eprintln!("Fatal: database '{}': {}", name, e);
        std::process::exit(1);
    })
}

fn pick_pg<'a>(
    databases: &'a HashMap<String, Arc<dyn Database>>,
    requested: &Option<String>,
) -> Arc<dyn Database> {
    let names: Vec<&str> = databases.keys().map(|s| s.as_str()).collect();
    let name = match requested {
        Some(n) => n.as_str(),
        None if names.len() == 1 => names[0],
        None => {
            eprintln!(
                "Multiple Postgres databases — specify one with --db. Available: {}",
                names.join(", ")
            );
            std::process::exit(1);
        }
    };
    databases.get(name).cloned().unwrap_or_else(|| {
        eprintln!(
            "Unknown database '{}'. Available: {}",
            name,
            databases.keys().cloned().collect::<Vec<_>>().join(", ")
        );
        std::process::exit(1);
    })
}

fn pick_mongo<'a>(
    databases: &'a HashMap<String, Arc<dyn MongoDatabase>>,
    requested: &Option<String>,
) -> Arc<dyn MongoDatabase> {
    let names: Vec<&str> = databases.keys().map(|s| s.as_str()).collect();
    let name = match requested {
        Some(n) => n.as_str(),
        None if names.len() == 1 => names[0],
        None => {
            eprintln!(
                "Multiple Mongo databases — specify one with --db. Available: {}",
                names.join(", ")
            );
            std::process::exit(1);
        }
    };
    databases.get(name).cloned().unwrap_or_else(|| {
        eprintln!(
            "Unknown mongo database '{}'. Available: {}",
            name,
            databases.keys().cloned().collect::<Vec<_>>().join(", ")
        );
        std::process::exit(1);
    })
}

// ── Sub-command handlers ──────────────────────────────────────────────────────

async fn run_serve(args: ServeArgs) {
    if args.max_rows == 0 {
        eprintln!("Fatal: --max-rows must be at least 1");
        std::process::exit(1);
    }

    let (pg_configs, mongo_configs) = match (&args.config, &args.database_url) {
        (Some(path), None) => {
            let c = Config::from_file(path).unwrap_or_else(|e| {
                eprintln!("Fatal: {}", e);
                std::process::exit(1);
            });
            (c.database, c.mongo)
        }
        (None, Some(url)) => (
            vec![DatabaseConfig {
                name: args.database_name.clone(),
                url: url.clone(),
                no_tls: args.no_tls,
            }],
            vec![],
        ),
        (Some(_), Some(_)) => {
            eprintln!("Fatal: --config and --database-url are mutually exclusive");
            std::process::exit(1);
        }
        (None, None) => {
            eprintln!("Fatal: provide --config <path> or --database-url <url>");
            std::process::exit(1);
        }
    };

    let mut databases: HashMap<String, Arc<dyn Database>> = HashMap::new();
    for cfg in pg_configs {
        let url = resolve_url(&cfg.name, &cfg.url);
        databases.insert(cfg.name.clone(), Arc::new(PostgresDb::new(&cfg.name, url, cfg.no_tls)));
    }

    let mut mongo_databases: HashMap<String, Arc<dyn MongoDatabase>> = HashMap::new();
    for cfg in mongo_configs {
        let uri = resolve_url(&cfg.name, &cfg.uri);
        let opts = ClientOptions::parse(&uri).await.unwrap_or_else(|e| {
            eprintln!("Fatal: mongo '{}' URI parse: {}", cfg.name, e);
            std::process::exit(1);
        });
        let dbname = opts.default_database.unwrap_or_else(|| {
            eprintln!("Fatal: mongo '{}' URI must include database name", cfg.name);
            std::process::exit(1);
        });
        mongo_databases.insert(cfg.name.clone(), Arc::new(MongoDb::new(&cfg.name, uri, dbname)));
    }

    server::run(databases, mongo_databases, args.max_rows).await;
}

async fn run_query(args: QueryArgs) {
    let sql = match args.sql {
        Some(s) => s,
        None => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf).unwrap_or_else(|e| {
                eprintln!("Failed to read SQL from stdin: {}", e);
                std::process::exit(1);
            });
            buf
        }
    };

    let sql = sql.trim().to_string();
    if sql.is_empty() {
        eprintln!("No SQL provided. Pass --sql '...' or pipe SQL to stdin.");
        std::process::exit(1);
    }

    let databases = build_pg_databases(&args.config).await;
    let db = pick_pg(&databases, &args.db);

    match db.execute_query(&sql, args.max_rows).await {
        Ok(result) => cli::print_pg_result(&result, args.json),
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}

async fn run_mongo(args: MongoArgs) {
    let operation = MongoOperation::from_str(&args.operation).unwrap_or_else(|e| {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    });

    let query = MongoQuery {
        collection: args.collection,
        operation,
        filter: args.filter,
        projection: args.projection,
        sort: args.sort,
        limit: args.limit,
        pipeline: args.pipeline,
        field: args.field,
    };

    let databases = build_mongo_databases(&args.config).await;
    let db = pick_mongo(&databases, &args.db);

    match db.execute_mongo(query).await {
        Ok(result) => cli::print_mongo_result(&result, args.json),
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}
