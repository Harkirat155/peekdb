#[tokio::main]
async fn main() {
    mcp_db_readonly::run().await;
}
