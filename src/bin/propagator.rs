use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let (_, handle) = ddcoin::run().await?;
    handle.await?;
    Ok(())
}
