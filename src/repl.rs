//! Interactive read-only REPL for exploring a configured target.

use crate::cli::{self, OutputFormat};
use crate::db::schema::{describe_pg_table, list_pg_tables, sample_pg_table};
use crate::db::traits::Database;
use crate::favorites::{Favorite, FavoritesFile};
use crate::paths;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::path::PathBuf;
use std::sync::Arc;

pub struct ReplArgs {
    pub config: Option<PathBuf>,
    pub db: Option<String>,
    pub format: OutputFormat,
}

pub async fn run_repl(
    databases: std::collections::HashMap<String, Arc<dyn Database>>,
    start_db: Option<String>,
    mut format: OutputFormat,
) {
    let mut names: Vec<String> = databases.keys().cloned().collect();
    names.sort();
    if names.is_empty() {
        eprintln!("Fatal: no PostgreSQL targets configured for repl (use query/mongo/redis for other backends)");
        std::process::exit(1);
    }

    let mut current = match start_db {
        Some(n) if databases.contains_key(&n) => n,
        Some(n) => {
            eprintln!("Unknown database '{n}'. Available: {}", names.join(", "));
            std::process::exit(1);
        }
        None if names.len() == 1 => names[0].clone(),
        None => names[0].clone(),
    };

    let history_path = paths::config_dir().join("history");
    let fav_path = FavoritesFile::default_path();
    let mut favorites = FavoritesFile::load(&fav_path).unwrap_or_default();

    let mut rl = DefaultEditor::new().unwrap_or_else(|e| {
        eprintln!("Fatal: readline init: {e}");
        std::process::exit(1);
    });
    let _ = rl.load_history(&history_path);

    println!("peekdb repl — read-only. Target: {current}");
    println!("Meta: \\q  \\dt  \\d name  \\c target  \\x  \\format  \\sample name  \\fav  \\h");
    println!("End SQL with ';'  (multi-line ok). Backend: postgres only in this session.");

    let mut buffer = String::new();

    loop {
        let prompt = if buffer.is_empty() {
            format!("peekdb:{current}> ")
        } else {
            format!("peekdb:{current}*> ")
        };
        let line = match rl.readline(&prompt) {
            Ok(l) => l,
            Err(ReadlineError::Interrupted) => {
                buffer.clear();
                println!("^C");
                continue;
            }
            Err(ReadlineError::Eof) => {
                println!();
                break;
            }
            Err(e) => {
                eprintln!("readline: {e}");
                break;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() && buffer.is_empty() {
            continue;
        }

        // Meta commands only at start of buffer
        if buffer.is_empty() && trimmed.starts_with('\\') {
            let _ = rl.add_history_entry(trimmed);
            if handle_meta(
                trimmed,
                &databases,
                &names,
                &mut current,
                &mut format,
                &mut favorites,
                &fav_path,
            )
            .await
            {
                break; // quit
            }
            continue;
        }

        if !buffer.is_empty() {
            buffer.push('\n');
        }
        buffer.push_str(&line);

        let ready = buffer.trim_end().ends_with(';')
            || (!buffer.contains(';') && !buffer.contains('\n') && !trimmed.is_empty() && !trimmed.starts_with('\\'));
        // Single-line without semicolon: execute on enter for ergonomics (psql needs ;)
        // Multi-line: wait for ;

        let is_multiline = buffer.lines().count() > 1 || buffer.contains('\n');
        let should_run = if is_multiline {
            buffer.trim_end().ends_with(';')
        } else {
            // one line — run if ends with ; OR is a complete single line SQL
            !trimmed.is_empty()
        };

        if !should_run && !ready {
            continue;
        }
        if is_multiline && !buffer.trim_end().ends_with(';') {
            continue;
        }

        let sql = buffer.trim().trim_end_matches(';').trim().to_string();
        buffer.clear();
        if sql.is_empty() {
            continue;
        }
        let _ = rl.add_history_entry(sql.as_str());

        let db = match databases.get(&current) {
            Some(d) => d.clone(),
            None => {
                eprintln!("target '{current}' gone");
                continue;
            }
        };
        match db.execute_query(&sql, 500).await {
            Ok(result) => cli::print_pg_result(&result, format),
            Err(e) => eprintln!("Error: {e}"),
        }
    }

    if let Some(parent) = history_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = rl.save_history(&history_path);
    println!("bye");
}

/// Returns true if the REPL should exit.
async fn handle_meta(
    cmd: &str,
    databases: &std::collections::HashMap<String, Arc<dyn Database>>,
    names: &[String],
    current: &mut String,
    format: &mut OutputFormat,
    favorites: &mut FavoritesFile,
    fav_path: &std::path::Path,
) -> bool {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    let head = parts[0].to_ascii_lowercase();
    match head.as_str() {
        "\\q" | "\\quit" => return true,
        "\\h" | "\\help" | "\\?" => {
            println!(
                "\\q                  quit\n\
                 \\dt                 list tables\n\
                 \\d NAME             describe table\n\
                 \\c NAME             switch postgres target\n\
                 \\x                  toggle vertical format\n\
                 \\format NAME        table|vertical|csv|tsv|json\n\
                 \\sample NAME [N]    sample N rows (default 10)\n\
                 \\fav                list favorites\n\
                 \\fav run NAME       run favorite\n\
                 \\fav save NAME      save last is not tracked; use body: \\fav save NAME = SQL\n\
                 \\targets            list targets"
            );
        }
        "\\targets" => {
            for n in names {
                let mark = if n == current { "*" } else { " " };
                println!("{mark} {n}");
            }
        }
        "\\c" => {
            if parts.len() < 2 {
                eprintln!("usage: \\c <target>");
            } else if databases.contains_key(parts[1]) {
                *current = parts[1].to_string();
                println!("switched to {current}");
            } else {
                eprintln!("unknown target. Available: {}", names.join(", "));
            }
        }
        "\\dt" => {
            let db = databases.get(current).unwrap().clone();
            match list_pg_tables(db.as_ref()).await {
                Ok(tables) => {
                    for t in tables {
                        println!("{t}");
                    }
                }
                Err(e) => eprintln!("Error: {e}"),
            }
        }
        "\\d" => {
            if parts.len() < 2 {
                eprintln!("usage: \\d <table>");
            } else {
                let db = databases.get(current).unwrap().clone();
                match describe_pg_table(db.as_ref(), parts[1]).await {
                    Ok(desc) => {
                        println!("{} ({})", desc.name, desc.kind);
                        for c in &desc.columns {
                            let null = c
                                .nullable
                                .map(|n| if n { "NULL" } else { "NOT NULL" })
                                .unwrap_or("");
                            println!("  {:<24} {:<20} {null}", c.name, c.data_type);
                        }
                        for i in &desc.indexes {
                            println!("  index: {} {:?}", i.name, i.definition);
                        }
                    }
                    Err(e) => eprintln!("Error: {e}"),
                }
            }
        }
        "\\x" => {
            *format = if *format == OutputFormat::Vertical {
                OutputFormat::Table
            } else {
                OutputFormat::Vertical
            };
            println!("format = {}", format.as_str());
        }
        "\\format" => {
            if parts.len() < 2 {
                println!("format = {}", format.as_str());
            } else {
                match parts[1].parse::<OutputFormat>() {
                    Ok(f) => {
                        *format = f;
                        println!("format = {}", format.as_str());
                    }
                    Err(e) => eprintln!("{e}"),
                }
            }
        }
        "\\sample" => {
            if parts.len() < 2 {
                eprintln!("usage: \\sample <table> [limit]");
            } else {
                let lim = parts
                    .get(2)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(10usize);
                let db = databases.get(current).unwrap().clone();
                match sample_pg_table(db.as_ref(), parts[1], lim).await {
                    Ok(r) => cli::print_pg_result(&r, *format),
                    Err(e) => eprintln!("Error: {e}"),
                }
            }
        }
        "\\fav" => {
            if parts.len() == 1 {
                for n in favorites.names() {
                    if let Some(f) = favorites.get(n) {
                        println!(
                            "{n:<16}  {:<8}  {}",
                            f.backend,
                            f.db.as_deref().unwrap_or("-")
                        );
                    }
                }
                if favorites.names().is_empty() {
                    println!("(no favorites — see \\h)");
                }
            } else if parts.get(1) == Some(&"run") && parts.len() >= 3 {
                let name = parts[2];
                match favorites.get(name) {
                    Some(f) if f.backend == "postgres" => {
                        if let Some(ref d) = f.db {
                            if databases.contains_key(d) {
                                *current = d.clone();
                            }
                        }
                        let db = databases.get(current).unwrap().clone();
                        match db.execute_query(&f.body, 500).await {
                            Ok(r) => cli::print_pg_result(&r, *format),
                            Err(e) => eprintln!("Error: {e}"),
                        }
                    }
                    Some(f) => eprintln!("favorite '{name}' backend is {} (repl is postgres)", f.backend),
                    None => eprintln!("unknown favorite '{name}'"),
                }
            } else if parts.get(1) == Some(&"save") && parts.len() >= 4 && parts[3] == "=" {
                // \fav save name = SELECT ...
                let name = parts[2];
                let body = parts[4..].join(" ");
                favorites.set(
                    name,
                    Favorite {
                        backend: "postgres".into(),
                        db: Some(current.clone()),
                        body,
                        notes: None,
                    },
                );
                match favorites.save(fav_path) {
                    Ok(()) => println!("saved favorite '{name}' → {}", fav_path.display()),
                    Err(e) => eprintln!("{e}"),
                }
            } else if parts.get(1) == Some(&"rm") && parts.len() >= 3 {
                if favorites.remove(parts[2]) {
                    let _ = favorites.save(fav_path);
                    println!("removed {}", parts[2]);
                } else {
                    eprintln!("unknown favorite");
                }
            } else {
                eprintln!("usage: \\fav | \\fav run NAME | \\fav save NAME = SQL | \\fav rm NAME");
            }
        }
        other => eprintln!("unknown meta command {other} (\\h for help)"),
    }
    false
}
