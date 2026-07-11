//! Black-box CLI tests. These spawn the built binary and assert on its stdout/stderr/exit
//! code — no live database required, so this file (unlike tests/integration_*.rs) is not
//! `#[ignore]`d and runs as part of the normal `cargo test`.

use assert_cmd::Command;
use predicates::prelude::*;
use std::io::Write;

fn bin() -> Command {
    Command::cargo_bin("mcp-db-readonly").unwrap()
}

#[test]
fn help_flag_succeeds() {
    bin()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("query"))
        .stdout(predicate::str::contains("mongo"))
        .stdout(predicate::str::contains("serve"));
}

#[test]
fn version_flag_succeeds() {
    bin().arg("--version").assert().success();
}

#[test]
fn query_help_succeeds() {
    bin()
        .args(["query", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--sql"));
}

#[test]
fn mongo_help_succeeds() {
    bin()
        .args(["mongo", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--operation"));
}

#[test]
fn query_missing_required_config_flag_fails() {
    bin()
        .args(["query", "--sql", "SELECT 1"])
        .assert()
        .failure();
}

#[test]
fn query_nonexistent_config_path_fails_with_clear_error() {
    bin()
        .args([
            "query",
            "--config",
            "/nonexistent/path/databases.toml",
            "--sql",
            "SELECT 1",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Cannot read"));
}

#[test]
fn query_malformed_toml_config_fails_with_clear_error() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(b"this is not [valid toml").unwrap();
    bin()
        .args([
            "query",
            "--config",
            f.path().to_str().unwrap(),
            "--sql",
            "SELECT 1",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Invalid config"));
}

#[test]
fn query_empty_config_fails_with_clear_error() {
    let f = tempfile::NamedTempFile::new().unwrap();
    bin()
        .args([
            "query",
            "--config",
            f.path().to_str().unwrap(),
            "--sql",
            "SELECT 1",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "no [[database]] or [[mongo]] entries",
        ));
}

#[test]
fn query_unknown_db_name_fails_with_available_list() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(
        br#"
        [[database]]
        name = "only-one"
        url = "postgres://user:pass@localhost:1/db"
        "#,
    )
    .unwrap();
    bin()
        .args([
            "query",
            "--config",
            f.path().to_str().unwrap(),
            "--db",
            "nope",
            "--sql",
            "SELECT 1",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Unknown database 'nope'"))
        .stderr(predicate::str::contains("only-one"));
}

#[test]
fn query_multiple_databases_without_db_flag_fails_ambiguous() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(
        br#"
        [[database]]
        name = "a"
        url = "postgres://user:pass@localhost:1/db"

        [[database]]
        name = "b"
        url = "postgres://user:pass@localhost:1/db"
        "#,
    )
    .unwrap();
    bin()
        .args([
            "query",
            "--config",
            f.path().to_str().unwrap(),
            "--sql",
            "SELECT 1",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--db"));
}

#[test]
fn mongo_unknown_operation_fails_with_clear_error() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(
        br#"
        [[mongo]]
        name = "only-one"
        uri = "mongodb://user:pass@localhost:1/db"
        "#,
    )
    .unwrap();
    bin()
        .args([
            "mongo",
            "--config",
            f.path().to_str().unwrap(),
            "--operation",
            "delete_everything",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Unknown operation"));
}

#[test]
fn query_empty_sql_fails_with_clear_error() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(
        br#"
        [[database]]
        name = "only-one"
        url = "postgres://user:pass@localhost:1/db"
        "#,
    )
    .unwrap();
    bin()
        .args([
            "query",
            "--config",
            f.path().to_str().unwrap(),
            "--sql",
            "   ",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("No SQL provided"));
}
