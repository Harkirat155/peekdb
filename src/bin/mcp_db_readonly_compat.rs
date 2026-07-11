//! Deprecated compat alias for the old `mcp-db-readonly` binary name. Existing MCP client
//! configs pointing at this binary keep working unmodified; new setups should use `peekdb`.
//! Planned for removal at 1.0.

#[tokio::main]
async fn main() {
    eprintln!(
        "warning: `mcp-db-readonly` is a deprecated alias for `peekdb` and will be removed \
         at 1.0 — please point your config at the `peekdb` binary instead."
    );
    peekdb::run().await;
}
