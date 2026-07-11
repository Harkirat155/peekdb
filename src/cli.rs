use crate::db::traits::{MongoResult, QueryResult};
use serde_json::Value;

pub fn print_pg_result(result: &QueryResult, json_mode: bool) {
    if json_mode {
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
    } else {
        if result.columns.is_empty() {
            if let Some(ref tag) = result.command_tag {
                eprintln!("{}", tag);
            }
        } else {
            print_table(&result.columns, &result.rows);
        }
    }

    let suffix = if result.truncated { ", truncated" } else { "" };
    eprintln!("({} rows{})", result.row_count, suffix);
}

pub fn print_mongo_result(result: &MongoResult, json_mode: bool) {
    if json_mode {
        let docs: Vec<Value> = result
            .documents
            .iter()
            .filter_map(|s| serde_json::from_str(s).ok())
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&docs).unwrap_or_default()
        );
    } else {
        for doc in &result.documents {
            match serde_json::from_str::<Value>(doc) {
                Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default()),
                Err(_) => println!("{}", doc),
            }
            println!("---");
        }
    }

    let suffix = if result.truncated { ", truncated" } else { "" };
    eprintln!("({} documents{})", result.count, suffix);
}

fn print_table(columns: &[String], rows: &[Vec<Option<String>>]) {
    const MAX_COL_WIDTH: usize = 80;

    // Widths are measured in chars, not bytes, so multi-byte UTF-8 content
    // (e.g. CJK/emoji) doesn't over-count against the column cap.
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

    // Header
    let mut header = "│".to_string();
    for (i, col) in columns.iter().enumerate() {
        let w = widths[i];
        let display = truncate(col, MAX_COL_WIDTH);
        header.push_str(&format!(" {:<width$} │", display, width = w));
    }
    println!("{}", header);

    println!("{}", border("├", "┼", "┤", "─"));

    // Rows
    for row in rows {
        let mut line = "│".to_string();
        for (i, cell) in row.iter().enumerate() {
            if i < widths.len() {
                let w = widths[i];
                let val = truncate(cell.as_deref().unwrap_or("NULL"), MAX_COL_WIDTH);
                line.push_str(&format!(" {:<width$} │", val, width = w));
            }
        }
        println!("{}", line);
    }

    println!("{}", border("└", "┴", "┘", "─"));
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", head)
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
        // Each CJK char is 3 bytes in UTF-8 — a byte-index slice at 79 would
        // land mid-character and panic. This is a regression test for that.
        let s = "日".repeat(85);
        let out = truncate(&s, 80);
        assert_eq!(out.chars().count(), 80);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn truncate_handles_string_shorter_than_ellipsis_budget() {
        // max=1 means saturating_sub(1) == 0 — must not panic on tiny max.
        let out = truncate("hello", 1);
        assert_eq!(out, "…");
    }
}
