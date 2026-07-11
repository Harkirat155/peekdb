pub mod audit;
pub mod cli;
pub mod config;
pub mod db;
pub mod paths;
pub mod server;
pub mod setup;

use audit::{AuditLog, AuditedDatabase, AuditedMongo, AuditedRedis};
use clap::{Parser, Subcommand};
use clap_complete::Shell;
use config::{interpolate_env, Config, DatabaseConfig, MongoConfig, RedisConfig};
use db::mongo::MongoDb;
use db::postgres::PostgresDb;
use db::redis::RedisDb;
use db::traits::{Database, MongoDatabase, MongoOperation, MongoQuery, RedisDatabase, RedisQuery};
use mongodb::options::ClientOptions;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

// ── Top-level CLI ─────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(
    name = "peekdb",
    about = "Read-only database access — direct CLI queries (query / mongo / redis) or MCP server (serve).\n\
             Running with no subcommand (or --config alone) defaults to 'serve' for backwards compatibility.",
    version
)]
pub struct Args {
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
    /// Execute a Redis command and print results
    Redis(RedisArgs),
    /// Create a starter databases.toml (default: ~/.config/peekdb/databases.toml)
    Init(InitArgs),
    /// Probe every configured target (SELECT 1 / list_collections / PING)
    Doctor(DoctorArgs),
    /// Inspect configuration
    #[command(subcommand)]
    Config(ConfigCmd),
    /// Print a ready-to-paste MCP client snippet (Claude / Cursor)
    McpSnippet(McpSnippetArgs),
    /// Generate shell completions (bash, zsh, fish, elvish, powershell)
    Completions(CompletionsArgs),
}

#[derive(clap::Args, Debug)]
struct InitArgs {
    /// Where to write the config (default: ~/.config/peekdb/databases.toml)
    #[arg(long)]
    path: Option<PathBuf>,
    /// Overwrite an existing file
    #[arg(long, default_value_t = false)]
    force: bool,
}

#[derive(clap::Args, Debug)]
struct DoctorArgs {
    /// Path to databases.toml (optional — discovered if omitted)
    #[arg(long)]
    config: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
enum ConfigCmd {
    /// List configured targets with secrets masked
    List(ConfigListArgs),
}

#[derive(clap::Args, Debug)]
struct ConfigListArgs {
    /// Path to databases.toml (optional — discovered if omitted)
    #[arg(long)]
    config: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
struct McpSnippetArgs {
    /// Path to databases.toml (optional — discovered if omitted)
    #[arg(long)]
    config: Option<PathBuf>,
    /// Absolute path to the peekdb binary (defaults to this executable)
    #[arg(long)]
    binary: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
struct CompletionsArgs {
    /// Shell to generate completions for
    shell: Shell,
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

    /// Path to the audit log (JSON Lines). Defaults to <config-dir>/audit.jsonl when
    /// --config is used; no default when using --database-url instead.
    #[arg(long, env = "MCP_AUDIT_LOG")]
    audit_log: Option<PathBuf>,

    /// Disable audit logging entirely for this run.
    #[arg(long, default_value_t = false)]
    no_audit_log: bool,
}

// ── Query sub-command (Postgres CLI) ─────────────────────────────────────────

#[derive(clap::Args, Debug)]
struct QueryArgs {
    /// Path to databases.toml (optional — discovered if omitted)
    #[arg(long)]
    config: Option<std::path::PathBuf>,

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

    /// Path to the audit log (JSON Lines). Defaults to <config-dir>/audit.jsonl.
    #[arg(long, env = "MCP_AUDIT_LOG")]
    audit_log: Option<PathBuf>,

    /// Disable audit logging entirely for this run.
    #[arg(long, default_value_t = false)]
    no_audit_log: bool,
}

// ── Mongo sub-command ─────────────────────────────────────────────────────────

#[derive(clap::Args, Debug)]
struct MongoArgs {
    /// Path to databases.toml (optional — discovered if omitted)
    #[arg(long)]
    config: Option<std::path::PathBuf>,

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

    /// Path to the audit log (JSON Lines). Defaults to <config-dir>/audit.jsonl.
    #[arg(long, env = "MCP_AUDIT_LOG")]
    audit_log: Option<PathBuf>,

    /// Disable audit logging entirely for this run.
    #[arg(long, default_value_t = false)]
    no_audit_log: bool,
}

// ── Redis sub-command ─────────────────────────────────────────────────────────

#[derive(clap::Args, Debug)]
struct RedisArgs {
    /// Path to databases.toml (optional — discovered if omitted)
    #[arg(long)]
    config: Option<std::path::PathBuf>,

    /// Redis instance name from config (can omit if only one is configured)
    #[arg(long)]
    db: Option<String>,

    /// Redis command to execute, e.g. GET, HGETALL, SMEMBERS (read-only commands only)
    command: String,

    /// Arguments to the command, in order (e.g. `redis --config c.toml HGET myhash field`)
    args: Vec<String>,

    /// Max items to display for list/set/hash-like results
    #[arg(long, default_value = "100")]
    limit: usize,

    /// Output as a JSON array instead of pretty-printed values
    #[arg(long)]
    json: bool,

    /// Path to the audit log (JSON Lines). Defaults to <config-dir>/audit.jsonl.
    #[arg(long, env = "MCP_AUDIT_LOG")]
    audit_log: Option<PathBuf>,

    /// Disable audit logging entirely for this run.
    #[arg(long, default_value_t = false)]
    no_audit_log: bool,
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Runs the full CLI: tracing init, arg parsing (with the legacy-serve-default shim),
/// and dispatch. Called by `src/main.rs`.
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
        Command::Redis(a) => run_redis(a).await,
        Command::Init(a) => setup::run_init(setup::InitArgs {
            path: a.path,
            force: a.force,
        }),
        Command::Doctor(a) => {
            setup::run_doctor(setup::DoctorArgs { config: a.config }).await
        }
        Command::Config(ConfigCmd::List(a)) => {
            setup::run_config_list(setup::ConfigListArgs { config: a.config })
        }
        Command::McpSnippet(a) => setup::run_mcp_snippet(setup::McpSnippetArgs {
            config: a.config,
            binary: a.binary,
        }),
        Command::Completions(a) => {
            setup::run_completions(setup::CompletionsArgs { shell: a.shell })
        }
    }
}

/// If the first positional argument is not a known sub-command name (or a help/
/// version flag), prepend "serve" so that bare `--config ...` still works.
/// Also covers zero-args invocations (e.g. an MCP client config that supplies
/// DATABASE_URL only via `env`, with no `args` at all) — those must default to
/// `serve` too, not fall through to clap's "missing subcommand" error.
fn inject_serve_default(mut args: Vec<String>) -> Vec<String> {
    const SUBCOMMANDS: &[&str] = &[
        "serve",
        "query",
        "mongo",
        "redis",
        "init",
        "doctor",
        "config",
        "mcp-snippet",
        "completions",
    ];
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
        for sub in [
            "serve",
            "query",
            "mongo",
            "redis",
            "init",
            "doctor",
            "config",
            "mcp-snippet",
            "completions",
        ] {
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

/// Where should the audit log live for this invocation? `--no-audit-log` wins outright;
/// otherwise an explicit `--audit-log`/`MCP_AUDIT_LOG` wins; otherwise default to
/// `<config-dir>/audit.jsonl` when a config file path is available (there's no sane
/// default directory for the ad-hoc `--database-url` shorthand, so that case stays
/// unaudited unless the user passes `--audit-log` explicitly).
fn resolve_audit_log_path(
    config_path: Option<&Path>,
    audit_log_flag: &Option<PathBuf>,
    no_audit_log: bool,
) -> Option<PathBuf> {
    if no_audit_log {
        return None;
    }
    if let Some(p) = audit_log_flag {
        return Some(p.clone());
    }
    config_path.map(|p| p.parent().unwrap_or(Path::new(".")).join("audit.jsonl"))
}

fn new_postgres_db(cfg: &DatabaseConfig, url: String) -> PostgresDb {
    let db = PostgresDb::new(&cfg.name, url, cfg.no_tls);
    match cfg.statement_timeout_ms {
        Some(ms) => db.with_statement_timeout_ms(ms),
        None => db,
    }
}

fn new_mongo_db(cfg: &MongoConfig, uri: String, dbname: String) -> MongoDb {
    let db = MongoDb::new(&cfg.name, uri, dbname);
    match cfg.query_timeout_ms {
        Some(ms) => db.with_query_timeout_ms(ms),
        None => db,
    }
}

fn new_redis_db(cfg: &RedisConfig, url: String) -> RedisDb {
    RedisDb::new(&cfg.name, url)
}

async fn build_pg_databases(
    path: &Path,
    source: &'static str,
    audit: &Option<Arc<AuditLog>>,
) -> HashMap<String, Arc<dyn Database>> {
    let config = load_config(path);
    let mut databases: HashMap<String, Arc<dyn Database>> = HashMap::new();
    for cfg in config.database {
        let url = resolve_url(&cfg.name, &cfg.url);
        let db: Arc<dyn Database> = Arc::new(new_postgres_db(&cfg, url));
        let db = match audit {
            Some(log) => Arc::new(AuditedDatabase::new(
                db,
                cfg.name.clone(),
                source,
                log.clone(),
            )) as Arc<dyn Database>,
            None => db,
        };
        databases.insert(cfg.name.clone(), db);
    }
    databases
}

async fn build_mongo_databases(
    path: &Path,
    source: &'static str,
    audit: &Option<Arc<AuditLog>>,
) -> HashMap<String, Arc<dyn MongoDatabase>> {
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
        let db: Arc<dyn MongoDatabase> = Arc::new(new_mongo_db(&cfg, uri, dbname));
        let db = match audit {
            Some(log) => Arc::new(AuditedMongo::new(db, cfg.name.clone(), source, log.clone()))
                as Arc<dyn MongoDatabase>,
            None => db,
        };
        dbs.insert(cfg.name.clone(), db);
    }
    dbs
}

async fn build_redis_databases(
    path: &Path,
    source: &'static str,
    audit: &Option<Arc<AuditLog>>,
) -> HashMap<String, Arc<dyn RedisDatabase>> {
    let config = load_config(path);
    let mut dbs: HashMap<String, Arc<dyn RedisDatabase>> = HashMap::new();
    for cfg in config.redis {
        let url = resolve_url(&cfg.name, &cfg.url);
        let db: Arc<dyn RedisDatabase> = Arc::new(new_redis_db(&cfg, url));
        let db = match audit {
            Some(log) => Arc::new(AuditedRedis::new(db, cfg.name.clone(), source, log.clone()))
                as Arc<dyn RedisDatabase>,
            None => db,
        };
        dbs.insert(cfg.name.clone(), db);
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

/// Redis counterpart of `pick_pg` — see its docs.
fn pick_redis(
    databases: &HashMap<String, Arc<dyn RedisDatabase>>,
    requested: &Option<String>,
) -> Result<Arc<dyn RedisDatabase>, String> {
    let names: Vec<&str> = databases.keys().map(|s| s.as_str()).collect();
    let name = match requested {
        Some(n) => n.as_str(),
        None if names.len() == 1 => names[0],
        None => {
            return Err(format!(
                "Multiple Redis instances — specify one with --db. Available: {}",
                names.join(", ")
            ));
        }
    };
    databases.get(name).cloned().ok_or_else(|| {
        format!(
            "Unknown redis instance '{}'. Available: {}",
            name,
            databases.keys().cloned().collect::<Vec<_>>().join(", ")
        )
    })
}

#[cfg(test)]
mod pick_tests {
    use super::*;
    use async_trait::async_trait;
    use db::traits::{DbError, MongoResult, QueryResult, RedisResult};

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

    struct StubRedis;
    #[async_trait]
    impl RedisDatabase for StubRedis {
        async fn execute_redis(&self, _query: RedisQuery) -> Result<RedisResult, DbError> {
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

    fn redis_map(names: &[&str]) -> HashMap<String, Arc<dyn RedisDatabase>> {
        names
            .iter()
            .map(|n| (n.to_string(), Arc::new(StubRedis) as Arc<dyn RedisDatabase>))
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

    #[test]
    fn pick_redis_autopicks_sole_instance() {
        let dbs = redis_map(&["only"]);
        assert!(pick_redis(&dbs, &None).is_ok());
    }

    #[test]
    fn pick_redis_rejects_unknown_name() {
        let dbs = redis_map(&["a"]);
        let err = pick_redis(&dbs, &Some("b".to_string())).err().unwrap();
        assert!(err.contains("Unknown redis instance 'b'"));
    }
}

// ── Sub-command handlers ──────────────────────────────────────────────────────

async fn run_serve(args: ServeArgs) {
    if args.max_rows == 0 {
        eprintln!("Fatal: --max-rows must be at least 1");
        std::process::exit(1);
    }

    // Resolve config path up front so discovery applies when neither flag is set.
    let discovered = if args.config.is_none() && args.database_url.is_none() {
        paths::resolve_config_path(None).ok()
    } else {
        None
    };
    let config_path = args.config.clone().or(discovered);

    let (pg_configs, mongo_configs, redis_configs) = match (&config_path, &args.database_url) {
        (Some(path), None) => {
            let c = Config::from_file(path).unwrap_or_else(|e| {
                eprintln!("Fatal: {}", e);
                std::process::exit(1);
            });
            (c.database, c.mongo, c.redis)
        }
        (None, Some(url)) => (
            vec![DatabaseConfig {
                name: args.database_name.clone(),
                url: url.clone(),
                no_tls: args.no_tls,
                allow_literal_password: true,
                statement_timeout_ms: None,
            }],
            vec![],
            vec![],
        ),
        (Some(_), Some(_)) => {
            eprintln!("Fatal: --config and --database-url are mutually exclusive");
            std::process::exit(1);
        }
        (None, None) => {
            eprintln!(
                "Fatal: provide --config <path>, --database-url <url>, or place a config at:\n{}",
                paths::search_paths()
                    .iter()
                    .map(|p| format!("  - {}", p.display()))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            std::process::exit(1);
        }
    };

    let audit_path =
        resolve_audit_log_path(config_path.as_deref(), &args.audit_log, args.no_audit_log);
    let audit: Option<Arc<AuditLog>> = audit_path.map(|p| Arc::new(AuditLog::open(p)));

    let mut databases: HashMap<String, Arc<dyn Database>> = HashMap::new();
    for cfg in pg_configs {
        let url = resolve_url(&cfg.name, &cfg.url);
        let db: Arc<dyn Database> = Arc::new(new_postgres_db(&cfg, url));
        let db = match &audit {
            Some(log) => Arc::new(AuditedDatabase::new(
                db,
                cfg.name.clone(),
                "mcp",
                log.clone(),
            )) as Arc<dyn Database>,
            None => db,
        };
        databases.insert(cfg.name.clone(), db);
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
        let db: Arc<dyn MongoDatabase> = Arc::new(new_mongo_db(&cfg, uri, dbname));
        let db = match &audit {
            Some(log) => Arc::new(AuditedMongo::new(db, cfg.name.clone(), "mcp", log.clone()))
                as Arc<dyn MongoDatabase>,
            None => db,
        };
        mongo_databases.insert(cfg.name.clone(), db);
    }

    let mut redis_databases: HashMap<String, Arc<dyn RedisDatabase>> = HashMap::new();
    for cfg in redis_configs {
        let url = resolve_url(&cfg.name, &cfg.url);
        let db: Arc<dyn RedisDatabase> = Arc::new(new_redis_db(&cfg, url));
        let db = match &audit {
            Some(log) => Arc::new(AuditedRedis::new(db, cfg.name.clone(), "mcp", log.clone()))
                as Arc<dyn RedisDatabase>,
            None => db,
        };
        redis_databases.insert(cfg.name.clone(), db);
    }

    server::run(databases, mongo_databases, redis_databases, args.max_rows).await;
}

fn resolve_cli_config(explicit: Option<&Path>) -> PathBuf {
    paths::resolve_config_path(explicit).unwrap_or_else(|e| {
        eprintln!("{}", e);
        std::process::exit(1);
    })
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

    let config_path = resolve_cli_config(args.config.as_deref());
    let audit_path =
        resolve_audit_log_path(Some(&config_path), &args.audit_log, args.no_audit_log);
    let audit: Option<Arc<AuditLog>> = audit_path.map(|p| Arc::new(AuditLog::open(p)));

    let databases = build_pg_databases(&config_path, "cli", &audit).await;
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

    let config_path = resolve_cli_config(args.config.as_deref());
    let audit_path =
        resolve_audit_log_path(Some(&config_path), &args.audit_log, args.no_audit_log);
    let audit: Option<Arc<AuditLog>> = audit_path.map(|p| Arc::new(AuditLog::open(p)));

    let databases = build_mongo_databases(&config_path, "cli", &audit).await;
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

async fn run_redis(args: RedisArgs) {
    let query = RedisQuery {
        command: args.command,
        args: args.args,
        limit: args.limit,
    };

    let config_path = resolve_cli_config(args.config.as_deref());
    let audit_path =
        resolve_audit_log_path(Some(&config_path), &args.audit_log, args.no_audit_log);
    let audit: Option<Arc<AuditLog>> = audit_path.map(|p| Arc::new(AuditLog::open(p)));

    let databases = build_redis_databases(&config_path, "cli", &audit).await;
    let db = pick_redis(&databases, &args.db).unwrap_or_else(|e| {
        eprintln!("{}", e);
        std::process::exit(1);
    });

    match db.execute_redis(query).await {
        Ok(result) => cli::print_redis_result(&result, args.json),
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}
