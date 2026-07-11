pub mod cli;
pub mod config;
pub mod db;
pub mod server;

use clap::{Parser, Subcommand};
use config::{interpolate_env, Config, DatabaseConfig};
use db::mongo::MongoDb;
use db::postgres::PostgresDb;
use db::traits::{Database, MongoDatabase, MongoOperation, MongoQuery};
use mongodb::options::ClientOptions;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

// ── Top-level CLI ─────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(
    name = "mcp-db-readonly",
    about = "Read-only database access — MCP server (serve) or direct CLI queries (query / mongo).\n\
             Running with no subcommand (or --config alone) defaults to 'serve' for backwards compatibility.",
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
    config: Option<std::path::PathBuf>,

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
    config: std::path::PathBuf,

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
    config: std::path::PathBuf,

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

/// Runs the full CLI: tracing init, arg parsing (with the legacy-serve-default shim),
/// and dispatch. Called by `src/main.rs` and by the `mcp-db-readonly` compat-alias binary.
pub async fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("RUST_LOG").unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

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
/// Also covers zero-args invocations (e.g. an MCP client config that supplies
/// DATABASE_URL only via `env`, with no `args` at all) — those must default to
/// `serve` too, not fall through to clap's "missing subcommand" error.
fn inject_serve_default(mut args: Vec<String>) -> Vec<String> {
    const SUBCOMMANDS: &[&str] = &["serve", "query", "mongo"];
    const PASSTHROUGH: &[&str] = &["--help", "-h", "--version", "-V"];

    match args.get(1).map(|s| s.as_str()) {
        None => args.push("serve".to_string()),
        Some(first) if !SUBCOMMANDS.contains(&first) && !PASSTHROUGH.contains(&first) => {
            args.insert(1, "serve".to_string());
        }
        _ => {}
    }
    args
}

#[cfg(test)]
mod inject_serve_default_tests {
    use super::inject_serve_default;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn zero_args_defaults_to_serve() {
        assert_eq!(inject_serve_default(v(&["bin"])), v(&["bin", "serve"]));
    }

    #[test]
    fn bare_config_flag_defaults_to_serve() {
        assert_eq!(
            inject_serve_default(v(&["bin", "--config", "databases.toml"])),
            v(&["bin", "serve", "--config", "databases.toml"])
        );
    }

    #[test]
    fn known_subcommand_first_is_untouched() {
        for sub in ["serve", "query", "mongo"] {
            assert_eq!(inject_serve_default(v(&["bin", sub])), v(&["bin", sub]));
        }
    }

    #[test]
    fn passthrough_flag_first_is_untouched() {
        for flag in ["--help", "-h", "--version", "-V"] {
            assert_eq!(inject_serve_default(v(&["bin", flag])), v(&["bin", flag]));
        }
    }

    #[test]
    fn unrecognized_first_arg_defaults_to_serve() {
        assert_eq!(
            inject_serve_default(v(&["bin", "--database-url", "postgres://x"])),
            v(&["bin", "serve", "--database-url", "postgres://x"])
        );
    }
}

// ── Shared database setup helpers ─────────────────────────────────────────────

async fn build_pg_databases(path: &Path) -> HashMap<String, Arc<dyn Database>> {
    let config = load_config(path);
    let mut databases: HashMap<String, Arc<dyn Database>> = HashMap::new();
    for cfg in config.database {
        let url = resolve_url(&cfg.name, &cfg.url);
        databases.insert(
            cfg.name.clone(),
            Arc::new(PostgresDb::new(&cfg.name, url, cfg.no_tls)),
        );
    }
    databases
}

async fn build_mongo_databases(path: &Path) -> HashMap<String, Arc<dyn MongoDatabase>> {
    let config = load_config(path);
    let mut dbs: HashMap<String, Arc<dyn MongoDatabase>> = HashMap::new();
    for cfg in config.mongo {
        let uri = resolve_url(&cfg.name, &cfg.uri);
        let opts = ClientOptions::parse(&uri).await.unwrap_or_else(|e| {
            eprintln!("Fatal: mongo '{}' URI parse: {}", cfg.name, e);
            std::process::exit(1);
        });
        let dbname = opts.default_database.unwrap_or_else(|| {
            eprintln!(
                "Fatal: mongo '{}' URI must include the database name",
                cfg.name
            );
            std::process::exit(1);
        });
        dbs.insert(
            cfg.name.clone(),
            Arc::new(MongoDb::new(&cfg.name, uri, dbname)),
        );
    }
    dbs
}

fn load_config(path: &Path) -> Config {
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

/// Picks the target Postgres database by name, or the sole configured one if
/// `requested` is None. Returns a user-facing error message on ambiguity/miss
/// rather than exiting directly, so callers (and tests) control the exit path.
fn pick_pg(
    databases: &HashMap<String, Arc<dyn Database>>,
    requested: &Option<String>,
) -> Result<Arc<dyn Database>, String> {
    let names: Vec<&str> = databases.keys().map(|s| s.as_str()).collect();
    let name = match requested {
        Some(n) => n.as_str(),
        None if names.len() == 1 => names[0],
        None => {
            return Err(format!(
                "Multiple Postgres databases — specify one with --db. Available: {}",
                names.join(", ")
            ));
        }
    };
    databases.get(name).cloned().ok_or_else(|| {
        format!(
            "Unknown database '{}'. Available: {}",
            name,
            databases.keys().cloned().collect::<Vec<_>>().join(", ")
        )
    })
}

/// Mongo counterpart of `pick_pg` — see its docs.
fn pick_mongo(
    databases: &HashMap<String, Arc<dyn MongoDatabase>>,
    requested: &Option<String>,
) -> Result<Arc<dyn MongoDatabase>, String> {
    let names: Vec<&str> = databases.keys().map(|s| s.as_str()).collect();
    let name = match requested {
        Some(n) => n.as_str(),
        None if names.len() == 1 => names[0],
        None => {
            return Err(format!(
                "Multiple Mongo databases — specify one with --db. Available: {}",
                names.join(", ")
            ));
        }
    };
    databases.get(name).cloned().ok_or_else(|| {
        format!(
            "Unknown mongo database '{}'. Available: {}",
            name,
            databases.keys().cloned().collect::<Vec<_>>().join(", ")
        )
    })
}

#[cfg(test)]
mod pick_tests {
    use super::*;
    use async_trait::async_trait;
    use db::traits::{DbError, MongoResult, QueryResult};

    struct StubDb;
    #[async_trait]
    impl Database for StubDb {
        async fn execute_query(
            &self,
            _sql: &str,
            _max_rows: usize,
        ) -> Result<QueryResult, DbError> {
            unimplemented!()
        }
    }

    struct StubMongo;
    #[async_trait]
    impl MongoDatabase for StubMongo {
        async fn execute_mongo(&self, _query: MongoQuery) -> Result<MongoResult, DbError> {
            unimplemented!()
        }
    }

    fn pg_map(names: &[&str]) -> HashMap<String, Arc<dyn Database>> {
        names
            .iter()
            .map(|n| (n.to_string(), Arc::new(StubDb) as Arc<dyn Database>))
            .collect()
    }

    fn mongo_map(names: &[&str]) -> HashMap<String, Arc<dyn MongoDatabase>> {
        names
            .iter()
            .map(|n| (n.to_string(), Arc::new(StubMongo) as Arc<dyn MongoDatabase>))
            .collect()
    }

    #[test]
    fn pick_pg_autopicks_sole_database() {
        let dbs = pg_map(&["only"]);
        assert!(pick_pg(&dbs, &None).is_ok());
    }

    #[test]
    fn pick_pg_requires_db_flag_when_multiple() {
        let dbs = pg_map(&["a", "b"]);
        let err = pick_pg(&dbs, &None).err().unwrap();
        assert!(err.contains("--db"));
    }

    #[test]
    fn pick_pg_rejects_unknown_name() {
        let dbs = pg_map(&["a"]);
        let err = pick_pg(&dbs, &Some("b".to_string())).err().unwrap();
        assert!(err.contains("Unknown database 'b'"));
    }

    #[test]
    fn pick_mongo_autopicks_sole_database() {
        let dbs = mongo_map(&["only"]);
        assert!(pick_mongo(&dbs, &None).is_ok());
    }

    #[test]
    fn pick_mongo_rejects_unknown_name() {
        let dbs = mongo_map(&["a"]);
        let err = pick_mongo(&dbs, &Some("b".to_string())).err().unwrap();
        assert!(err.contains("Unknown mongo database 'b'"));
    }
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
        databases.insert(
            cfg.name.clone(),
            Arc::new(PostgresDb::new(&cfg.name, url, cfg.no_tls)),
        );
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
        mongo_databases.insert(
            cfg.name.clone(),
            Arc::new(MongoDb::new(&cfg.name, uri, dbname)),
        );
    }

    server::run(databases, mongo_databases, args.max_rows).await;
}

async fn run_query(args: QueryArgs) {
    let sql = match args.sql {
        Some(s) => s,
        None => {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .unwrap_or_else(|e| {
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
    let db = pick_pg(&databases, &args.db).unwrap_or_else(|e| {
        eprintln!("{}", e);
        std::process::exit(1);
    });

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
    let db = pick_mongo(&databases, &args.db).unwrap_or_else(|e| {
        eprintln!("{}", e);
        std::process::exit(1);
    });

    match db.execute_mongo(query).await {
        Ok(result) => cli::print_mongo_result(&result, args.json),
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}
