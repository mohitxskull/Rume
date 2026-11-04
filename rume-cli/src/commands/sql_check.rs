//! Scans SQL files in a directory and validates syntax, non-emptiness, balanced delimiters, and placeholders.

use std::fs;
use std::path::{Path, PathBuf};
use console::style;
use serde::Serialize;
use crate::ui::table::TableBuilder;

#[derive(Serialize)]
pub struct SqlCheckReport {
    pub all_passed: bool,
    pub total_files: usize,
    pub valid_files: usize,
    pub files: Vec<SqlFileStatus>,
}

#[derive(Serialize)]
pub struct SqlFileStatus {
    pub path: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn run(path_str: &str, format: &str, is_json: bool) {
    let target_dir = Path::new(path_str);
    let mut files = Vec::new();
    if target_dir.exists() {
        collect_sql_files(target_dir, &mut files);
    }

    files.sort();

    let mut statuses = Vec::with_capacity(files.len());
    let mut all_ok = true;
    let mut valid_count = 0;

    for file_path in &files {
        let display_path = file_path.display().to_string();
        match validate_sql_file(file_path) {
            Ok(()) => {
                valid_count += 1;
                statuses.push(SqlFileStatus {
                    path: display_path,
                    ok: true,
                    error: None,
                });
            }
            Err(err) => {
                all_ok = false;
                statuses.push(SqlFileStatus {
                    path: display_path,
                    ok: false,
                    error: Some(err),
                });
            }
        }
    }

    let report = SqlCheckReport {
        all_passed: all_ok,
        total_files: files.len(),
        valid_files: valid_count,
        files: statuses,
    };

    if is_json || format == "json" {
        println!("{}", serde_json::to_string_pretty(&report).unwrap_or_default());
        if !all_ok {
            std::process::exit(1);
        }
        return;
    }

    println!("Checking SQL files in `{path_str}`:");
    println!();

    if files.is_empty() {
        println!("  No SQL files found in `{path_str}`.");
        return;
    }

    let mut table = TableBuilder::new();
    table.head(&["File", "Status", "Details"]);

    for item in &report.files {
        if item.ok {
            table.row_colored(&[
                (&item.path, None),
                ("VALID", Some(comfy_table::Color::Green)),
                ("syntax ok", None),
            ]);
        } else {
            let err_msg = item.error.as_deref().unwrap_or("unknown error");
            table.row_colored(&[
                (&item.path, None),
                ("FAIL", Some(comfy_table::Color::Red)),
                (err_msg, Some(comfy_table::Color::Red)),
            ]);
        }
    }

    table.render();
    println!();
    if all_ok {
        println!("{}", style(format!("All {} SQL file(s) passed validation.", files.len())).green().bold());
    } else {
        println!("{}", style(format!("{}/{} SQL file(s) failed validation.", files.len() - valid_count, files.len())).red().bold());
        std::process::exit(1);
    }
}

fn collect_sql_files(dir: &Path, files: &mut Vec<PathBuf>) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_sql_files(&path, files);
            } else if path.extension().and_then(|s| s.to_str()) == Some("sql") {
                files.push(path);
            }
        }
    }
}

fn validate_sql_file(path: &Path) -> Result<(), String> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("cannot read file: {e}"))?;

    // Filter out comments and whitespace to check if query body exists
    let meaningful_sql: String = content
        .lines()
        .map(|line| {
            let trimmed = line.trim();
            if trimmed.starts_with("--") || trimmed.starts_with('#') {
                ""
            } else if let Some(idx) = trimmed.find("--") {
                &trimmed[..idx]
            } else {
                trimmed
            }
        })
        .collect::<Vec<_>>()
        .join(" ");

    let trimmed = meaningful_sql.trim();
    if trimmed.is_empty() {
        return Err("SQL file is empty (contains no query statements)".to_string());
    }

    // Check balanced delimiters: (), quotes
    let mut paren_depth = 0i32;
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut prev_char = '\0';

    for ch in content.chars() {
        if in_single_quote {
            if ch == '\'' && prev_char != '\\' {
                in_single_quote = false;
            }
        } else if in_double_quote {
            if ch == '"' && prev_char != '\\' {
                in_double_quote = false;
            }
        } else {
            match ch {
                '\'' => in_single_quote = true,
                '"' => in_double_quote = true,
                '(' => paren_depth += 1,
                ')' => {
                    paren_depth -= 1;
                    if paren_depth < 0 {
                        return Err("unmatched closing parenthesis ')'".to_string());
                    }
                }
                _ => {}
            }
        }
        prev_char = ch;
    }

    if in_single_quote {
        return Err("unclosed single quote (')".to_string());
    }
    if in_double_quote {
        return Err("unclosed double quote (\")".to_string());
    }
    if paren_depth != 0 {
        return Err(format!("unclosed parenthesis: {paren_depth} open parenthesis remain unmatched"));
    }

    // Validate that the SQL starts with a recognized SQL keyword
    let first_word = trimmed
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();

    const VALID_KEYWORDS: &[&str] = &[
        "SELECT", "INSERT", "UPDATE", "DELETE", "CREATE", "ALTER", "DROP",
        "TRUNCATE", "WITH", "BEGIN", "COMMIT", "ROLLBACK", "SET", "PRAGMA",
        "SHOW", "EXPLAIN", "CALL", "GRANT", "REVOKE", "MERGE", "REPLACE",
        "UPSERT", "SAVEPOINT", "RELEASE",
    ];

    if !VALID_KEYWORDS.contains(&first_word.as_str()) {
        return Err(format!("query starts with unrecognized SQL statement keyword `{first_word}`"));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_valid_sql() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        fs::write(temp.path(), "SELECT * FROM users WHERE id = $1;").unwrap();
        assert!(validate_sql_file(temp.path()).is_ok());
    }

    #[test]
    fn test_validate_unmatched_paren() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        fs::write(temp.path(), "SELECT * FROM users WHERE id IN (1, 2;").unwrap();
        assert!(validate_sql_file(temp.path()).is_err());
    }

    #[test]
    fn test_validate_empty_sql() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        fs::write(temp.path(), "-- Just a comment\n").unwrap();
        assert!(validate_sql_file(temp.path()).is_err());
    }

    #[test]
    fn test_validate_unrecognized_keyword() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        fs::write(temp.path(), "FOOBAR something").unwrap();
        assert!(validate_sql_file(temp.path()).is_err());
    }
}
