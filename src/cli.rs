use crate::db::traits::{MongoResult, QueryResult, RedisResult};
use serde_json::Value;
use std::str::FromStr;

/// How CLI/REPL results are rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    #[default]
    Table,
    Vertical,
    Csv,
    Tsv,
    Json,
}

impl FromStr for OutputFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "table" | "t" => Ok(Self::Table),
            "vertical" | "x" | "expanded" => Ok(Self::Vertical),
            "csv" => Ok(Self::Csv),
            "tsv" => Ok(Self::Tsv),
            "json" | "j" => Ok(Self::Json),
            other => Err(format!(
                "unknown format '{other}' (table|vertical|csv|tsv|json)"
            )),
        }
    }
}

impl OutputFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Table => "table",
            Self::Vertical => "vertical",
            Self::Csv => "csv",
            Self::Tsv => "tsv",
            Self::Json => "json",
        }
    }
}

pub fn print_pg_result(result: &QueryResult, format: OutputFormat) {
    match format {
        OutputFormat::Json => print_pg_json(result),
        OutputFormat::Table => {
            if result.columns.is_empty() {
                if let Some(ref tag) = result.command_tag {
                    eprintln!("{tag}");
                }
            } else {
                print_table(&result.columns, &result.rows);
            }
        }
        OutputFormat::Vertical => print_vertical(&result.columns, &result.rows),
        OutputFormat::Csv => print_delimited(&result.columns, &result.rows, ','),
        OutputFormat::Tsv => print_delimited(&result.columns, &result.rows, '\t'),
    }
    let suffix = if result.truncated { ", truncated" } else { "" };
    let red = if result.redacted { ", redacted" } else { "" };
    eprintln!("({} rows{}{})", result.row_count, suffix, red);
}

pub fn print_mongo_result(result: &MongoResult, format: OutputFormat) {
    match format {
        OutputFormat::Json => {
            let docs: Vec<Value> = result
                .documents
                .iter()
                .filter_map(|s| serde_json::from_str(s).ok())
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&docs).unwrap_or_default()
            );
        }
        OutputFormat::Csv | OutputFormat::Tsv => {
            // Flatten shallow keys across docs into a table when possible.
            if let Some((cols, rows)) = mongo_as_table(result) {
                let sep = if matches!(format, OutputFormat::Csv) {
                    ','
                } else {
                    '\t'
                };
                print_delimited(&cols, &rows, sep);
            } else {
                for doc in &result.documents {
                    println!("{doc}");
                }
            }
        }
        OutputFormat::Vertical => {
            if let Some((cols, rows)) = mongo_as_table(result) {
                print_vertical(&cols, &rows);
            } else {
                for (i, doc) in result.documents.iter().enumerate() {
                    println!("-[ RECORD {} ]-", i + 1);
                    println!("{doc}");
                }
            }
        }
        OutputFormat::Table => {
            if let Some((cols, rows)) = mongo_as_table(result) {
                print_table(&cols, &rows);
            } else {
                for doc in &result.documents {
                    match serde_json::from_str::<Value>(doc) {
                        Ok(v) => {
                            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default())
                        }
                        Err(_) => println!("{doc}"),
                    }
                    println!("---");
                }
            }
        }
    }
    let suffix = if result.truncated { ", truncated" } else { "" };
    let red = if result.redacted { ", redacted" } else { "" };
    eprintln!("({} documents{}{})", result.count, suffix, red);
}

pub fn print_redis_result(result: &RedisResult, format: OutputFormat) {
    match format {
        OutputFormat::Json => {
            let values: Vec<Value> = result
                .values
                .iter()
                .map(|s| serde_json::from_str(s).unwrap_or(Value::String(s.clone())))
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&values).unwrap_or_default()
            );
        }
        OutputFormat::Csv | OutputFormat::Tsv => {
            let sep = if matches!(format, OutputFormat::Csv) {
                ','
            } else {
                '\t'
            };
            println!("value");
            for v in &result.values {
                println!("{}", escape_delim(v, sep));
            }
        }
        OutputFormat::Vertical | OutputFormat::Table => {
            for (i, v) in result.values.iter().enumerate() {
                if matches!(format, OutputFormat::Vertical) {
                    println!("-[ RECORD {} ]-", i + 1);
                }
                match serde_json::from_str::<Value>(v) {
                    Ok(parsed) => println!(
                        "{}",
                        serde_json::to_string_pretty(&parsed).unwrap_or_default()
                    ),
                    Err(_) => println!("{v}"),
                }
            }
        }
    }
    let suffix = if result.truncated { ", truncated" } else { "" };
    let red = if result.redacted { ", redacted" } else { "" };
    eprintln!("({} value(s){}{})", result.count, suffix, red);
}

fn print_pg_json(result: &QueryResult) {
    let rows: Vec<Value> = result
        .rows
        .iter()
        .map(|row| {
            let mut obj = serde_json::Map::new();
            for (i, col) in result.columns.iter().enumerate() {
                obj.insert(
                    col.clone(),
                    match row.get(i).and_then(|v| v.as_deref()) {
                        Some(v) => Value::String(v.to_string()),
                        None => Value::Null,
                    },
                );
            }
            Value::Object(obj)
        })
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&rows).unwrap_or_default()
    );
}

type StringTable = (Vec<String>, Vec<Vec<Option<String>>>);

fn mongo_as_table(result: &MongoResult) -> Option<StringTable> {
    let mut cols: Vec<String> = Vec::new();
    let mut maps: Vec<serde_json::Map<String, Value>> = Vec::new();
    for doc in &result.documents {
        let v: Value = serde_json::from_str(doc).ok()?;
        let obj = v.as_object()?.clone();
        for k in obj.keys() {
            if !cols.contains(k) {
                cols.push(k.clone());
            }
        }
        maps.push(obj);
    }
    if cols.is_empty() {
        return None;
    }
    cols.sort();
    let rows: Vec<Vec<Option<String>>> = maps
        .iter()
        .map(|m| {
            cols.iter()
                .map(|c| {
                    m.get(c).map(|v| match v {
                        Value::String(s) => s.clone(),
                        Value::Null => "NULL".into(),
                        other => other.to_string(),
                    })
                })
                .collect()
        })
        .collect();
    Some((cols, rows))
}

fn print_vertical(columns: &[String], rows: &[Vec<Option<String>>]) {
    if rows.is_empty() {
        println!("(0 rows)");
        return;
    }
    let label_w = columns.iter().map(|c| c.chars().count()).max().unwrap_or(0);
    for (ri, row) in rows.iter().enumerate() {
        println!("-[ RECORD {} ]-", ri + 1);
        for (i, col) in columns.iter().enumerate() {
            let val = row
                .get(i)
                .and_then(|c| c.as_deref())
                .unwrap_or("NULL");
            println!("{col:<label_w$} | {val}");
        }
    }
}

fn print_delimited(columns: &[String], rows: &[Vec<Option<String>>], sep: char) {
    println!(
        "{}",
        columns
            .iter()
            .map(|c| escape_delim(c, sep))
            .collect::<Vec<_>>()
            .join(&sep.to_string())
    );
    for row in rows {
        let line: Vec<String> = columns
            .iter()
            .enumerate()
            .map(|(i, _)| {
                let cell = row
                    .get(i)
                    .and_then(|c| c.as_deref())
                    .unwrap_or("");
                escape_delim(cell, sep)
            })
            .collect();
        println!("{}", line.join(&sep.to_string()));
    }
}

fn escape_delim(s: &str, sep: char) -> String {
    if s.contains(sep) || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn print_table(columns: &[String], rows: &[Vec<Option<String>>]) {
    const MAX_COL_WIDTH: usize = 80;

    let mut widths: Vec<usize> = columns
        .iter()
        .map(|c| c.chars().count().min(MAX_COL_WIDTH))
        .collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < widths.len() {
                let len = cell
                    .as_deref()
                    .unwrap_or("NULL")
                    .chars()
                    .count()
                    .min(MAX_COL_WIDTH);
                widths[i] = widths[i].max(len);
            }
        }
    }

    let border = |left: &str, mid: &str, right: &str, fill: &str| {
        let mut s = left.to_string();
        for (i, w) in widths.iter().enumerate() {
            s.push_str(&fill.repeat(w + 2));
            if i < widths.len() - 1 {
                s.push_str(mid);
            } else {
                s.push_str(right);
            }
        }
        s
    };

    println!("{}", border("┌", "┬", "┐", "─"));

    let mut header = "│".to_string();
    for (i, col) in columns.iter().enumerate() {
        let w = widths[i];
        let display = truncate(col, MAX_COL_WIDTH);
        header.push_str(&format!(" {:<width$} │", display, width = w));
    }
    println!("{header}");

    println!("{}", border("├", "┼", "┤", "─"));

    for row in rows {
        let mut line = "│".to_string();
        for (i, cell) in row.iter().enumerate() {
            if i < widths.len() {
                let w = widths[i];
                let val = truncate(cell.as_deref().unwrap_or("NULL"), MAX_COL_WIDTH);
                line.push_str(&format!(" {:<width$} │", val, width = w));
            }
        }
        println!("{line}");
    }

    println!("{}", border("└", "┴", "┘", "─"));
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_ascii_under_limit_is_unchanged() {
        assert_eq!(truncate("hello", 80), "hello");
    }

    #[test]
    fn truncate_ascii_over_limit_gets_ellipsis() {
        let s = "a".repeat(85);
        let out = truncate(&s, 80);
        assert_eq!(out.chars().count(), 80);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn truncate_multibyte_utf8_does_not_panic() {
        let s = "日".repeat(85);
        let out = truncate(&s, 80);
        assert_eq!(out.chars().count(), 80);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn truncate_handles_string_shorter_than_ellipsis_budget() {
        let out = truncate("hello", 1);
        assert_eq!(out, "…");
    }

    #[test]
    fn output_format_parses_aliases() {
        assert_eq!(OutputFormat::from_str("json").unwrap(), OutputFormat::Json);
        assert_eq!(OutputFormat::from_str("x").unwrap(), OutputFormat::Vertical);
        assert_eq!(OutputFormat::from_str("csv").unwrap(), OutputFormat::Csv);
        assert!(OutputFormat::from_str("nope").is_err());
    }

    #[test]
    fn escape_delim_quotes_when_needed() {
        assert_eq!(escape_delim("a,b", ','), "\"a,b\"");
        assert_eq!(escape_delim("plain", ','), "plain");
    }

    #[test]
    fn print_delimited_csv_roundtrip_shape() {
        let cols = vec!["id".into(), "name".into()];
        let rows = vec![vec![Some("1".into()), Some("a,b".into())]];
        // smoke: doesn't panic
        print_delimited(&cols, &rows, ',');
    }
}
