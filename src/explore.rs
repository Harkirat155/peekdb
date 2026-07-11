//! Lightweight interactive explorer (menu-driven) over postgres targets.
//! Reuses schema discovery APIs — no parallel connection path.

use crate::cli::{self, OutputFormat};
use crate::db::schema::{describe_pg_table, list_pg_tables, sample_pg_table};
use crate::db::traits::Database;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::collections::HashMap;
use std::sync::Arc;

pub async fn run_explore(
    databases: HashMap<String, Arc<dyn Database>>,
    start_db: Option<String>,
) {
    let mut names: Vec<String> = databases.keys().cloned().collect();
    names.sort();
    if names.is_empty() {
        eprintln!("Fatal: no PostgreSQL targets for explore");
        std::process::exit(1);
    }

    let mut current = match start_db {
        Some(n) if databases.contains_key(&n) => n,
        Some(n) => {
            eprintln!("Unknown database '{n}'. Available: {}", names.join(", "));
            std::process::exit(1);
        }
        None => names[0].clone(),
    };

    let mut rl = DefaultEditor::new().unwrap_or_else(|e| {
        eprintln!("Fatal: readline: {e}");
        std::process::exit(1);
    });

    println!("peekdb explore — interactive (read-only)");
    println!("Commands: t=targets  l=list tables  d <table>  s <table> [n]  q=quit  ?=help");

    loop {
        let prompt = format!("explore:{current}> ");
        let line = match rl.readline(&prompt) {
            Ok(l) => l,
            Err(ReadlineError::Interrupted | ReadlineError::Eof) => {
                println!();
                break;
            }
            Err(e) => {
                eprintln!("{e}");
                break;
            }
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let _ = rl.add_history_entry(line);
        let parts: Vec<&str> = line.split_whitespace().collect();
        match parts[0] {
            "q" | "quit" | "exit" => break,
            "?" | "h" | "help" => {
                println!(
                    "t              list targets / switch: t <name>\n\
                     l              list tables\n\
                     d <table>      describe\n\
                     s <table> [n]  sample n rows (default 10)\n\
                     q              quit"
                );
            }
            "t" => {
                if parts.len() >= 2 {
                    if databases.contains_key(parts[1]) {
                        current = parts[1].to_string();
                        println!("target = {current}");
                    } else {
                        eprintln!("unknown. Available: {}", names.join(", "));
                    }
                } else {
                    for n in &names {
                        let m = if n == &current { "*" } else { " " };
                        println!("{m} {n}");
                    }
                }
            }
            "l" => {
                let db = databases.get(&current).unwrap().clone();
                match list_pg_tables(db.as_ref()).await {
                    Ok(tables) => {
                        if tables.is_empty() {
                            println!("(no tables or all filtered by allowlist)");
                        }
                        for (i, t) in tables.iter().enumerate() {
                            println!("{:>3}  {t}", i + 1);
                        }
                    }
                    Err(e) => eprintln!("Error: {e}"),
                }
            }
            "d" => {
                if parts.len() < 2 {
                    eprintln!("usage: d <table>");
                    continue;
                }
                let db = databases.get(&current).unwrap().clone();
                match describe_pg_table(db.as_ref(), parts[1]).await {
                    Ok(desc) => {
                        println!("{} — {}", desc.name, desc.kind);
                        for c in desc.columns {
                            println!(
                                "  {:<28} {}",
                                c.name,
                                c.data_type
                            );
                        }
                    }
                    Err(e) => eprintln!("Error: {e}"),
                }
            }
            "s" => {
                if parts.len() < 2 {
                    eprintln!("usage: s <table> [limit]");
                    continue;
                }
                let lim = parts.get(2).and_then(|x| x.parse().ok()).unwrap_or(10);
                let db = databases.get(&current).unwrap().clone();
                match sample_pg_table(db.as_ref(), parts[1], lim).await {
                    Ok(r) => cli::print_pg_result(&r, OutputFormat::Table),
                    Err(e) => eprintln!("Error: {e}"),
                }
            }
            other => eprintln!("unknown command '{other}' (? for help)"),
        }
    }
    println!("bye");
}
