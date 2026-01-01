use anyhow::Result;
use cvc_mcp::server;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    server::run().await
}
