//! Black-box CLI tests. These spawn the built binary and assert on its stdout/stderr/exit
//! code — no live database required, so this file (unlike tests/integration_*.rs) is not
//! `#[ignore]`d and runs as part of the normal `cargo test`.

use assert_cmd::Command;
use predicates::prelude::*;
use std::io::Write;

fn bin() -> Command {
    Command::cargo_bin("peekdb").unwrap()
}

#[test]
fn help_flag_succeeds() {
    bin()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("query"))
        .stdout(predicate::str::contains("mongo"))
        .stdout(predicate::str::contains("redis"))
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
fn redis_help_succeeds() {
    bin()
        .args(["redis", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("COMMAND"));
}

#[test]
fn redis_disallowed_command_is_rejected_without_needing_a_live_server() {
    // check_allowed_redis runs before the connection attempt, so this doesn't need a
    // live Redis — same shape as the Postgres/Mongo "no live DB needed" CLI tests.
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(
        br#"
        [[redis]]
        name = "only-one"
        url = "redis://localhost:1"
        "#,
    )
    .unwrap();
    bin()
        .args([
            "redis",
            "--config",
            f.path().to_str().unwrap(),
            "SET",
            "foo",
            "bar",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Blocked"));
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
        .stderr(predicate::str::contains("no [[database]]"))
        .stderr(predicate::str::contains("[[redis]]"));
}

#[test]
fn query_unknown_db_name_fails_with_available_list() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(
        br#"
        [[database]]
        name = "only-one"
        url = "postgres://user@localhost:1/db"
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
        url = "postgres://user@localhost:1/db"

        [[database]]
        name = "b"
        url = "postgres://user@localhost:1/db"
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
        uri = "mongodb://user@localhost:1/db"
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
fn query_literal_password_in_config_fails_with_clear_error() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(
        br#"
        [[database]]
        name = "staging"
        url = "postgres://app:hunter2@staging-db.example.com/app"
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
        .stderr(predicate::str::contains("literal password"))
        .stderr(predicate::str::contains("allow_literal_password"));
}

#[test]
fn query_literal_password_with_explicit_opt_out_is_accepted_past_config_load() {
    // Doesn't need a live DB — just confirms validation lets it through and execution
    // proceeds to the (failing, since there's no real server) connection attempt rather
    // than being rejected at config-load time.
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(
        br#"
        [[database]]
        name = "local"
        url = "postgres://postgres:postgres@localhost:1/db"
        allow_literal_password = true
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
        .stderr(predicate::str::contains("Connection failed").or(predicate::str::contains("Error")))
        .stderr(predicate::str::contains("literal password").not());
}

#[test]
fn query_writes_an_audit_log_entry_even_on_connection_failure() {
    // No live DB needed: port 1 refuses the connection immediately, but the audit
    // wrapper covers the whole call including connection failures, not just successes.
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("databases.toml");
    std::fs::write(
        &config_path,
        br#"
        [[database]]
        name = "only-one"
        url = "postgres://user@localhost:1/db"
        "#,
    )
    .unwrap();
    let audit_path = dir.path().join("audit.jsonl");

    bin()
        .args([
            "query",
            "--config",
            config_path.to_str().unwrap(),
            "--audit-log",
            audit_path.to_str().unwrap(),
            "--sql",
            "SELECT 1",
        ])
        .assert()
        .failure();

    let contents = std::fs::read_to_string(&audit_path).unwrap();
    assert_eq!(contents.lines().count(), 1);
    let parsed: serde_json::Value = serde_json::from_str(contents.lines().next().unwrap()).unwrap();
    assert_eq!(parsed["source"], "cli");
    assert_eq!(parsed["backend"], "postgres");
    assert_eq!(parsed["outcome"], "error");
    assert_eq!(parsed["target"], "only-one");
    // The audit line must never carry the connection string — only the config entry name.
    assert!(!contents.contains("postgres://"));
    assert!(!contents.to_lowercase().contains("localhost"));
}

#[test]
fn query_no_audit_log_flag_suppresses_the_default_audit_file() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("databases.toml");
    std::fs::write(
        &config_path,
        br#"
        [[database]]
        name = "only-one"
        url = "postgres://user@localhost:1/db"
        "#,
    )
    .unwrap();

    bin()
        .args([
            "query",
            "--config",
            config_path.to_str().unwrap(),
            "--no-audit-log",
            "--sql",
            "SELECT 1",
        ])
        .assert()
        .failure();

    assert!(!dir.path().join("audit.jsonl").exists());
}

#[test]
fn query_defaults_to_an_audit_file_next_to_the_config_when_no_flag_given() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("databases.toml");
    std::fs::write(
        &config_path,
        br#"
        [[database]]
        name = "only-one"
        url = "postgres://user@localhost:1/db"
        "#,
    )
    .unwrap();

    bin()
        .args([
            "query",
            "--config",
            config_path.to_str().unwrap(),
            "--sql",
            "SELECT 1",
        ])
        .assert()
        .failure();

    assert!(dir.path().join("audit.jsonl").exists());
}

#[test]
fn query_empty_sql_fails_with_clear_error() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(
        br#"
        [[database]]
        name = "only-one"
        url = "postgres://user@localhost:1/db"
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
