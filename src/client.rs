use crate::{
    daemon::Config,
    install,
    protocol::{Reply, Request},
    transport,
};
use anyhow::{Context, Result};
use std::time::Duration;
use tokio::time::{sleep, timeout};

/// The process id of the running service.
pub async fn service_pid(config: &Config) -> Result<u32> {
    let mut stream = transport::connect(&config.socket()).await?;
    transport::write_frame(&mut stream, &Request::Health).await?;
    match transport::read_frame(&mut stream).await? {
        Reply::Health { pid } => Ok(pid),
        _ => anyhow::bail!("Unexpected daemon health response"),
    }
}
async fn probe(config: &Config) -> Result<()> {
    service_pid(config).await.map(|_| ())
}
pub async fn ensure_running(config: &Config) -> Result<()> {
    if probe(config).await.is_ok() {
        return Ok(());
    }
    anyhow::ensure!(
        config.supervised,
        "No bridge is running for {}. Start one in a shell: session-bridge daemon",
        config.state.display()
    );
    install::start_service().await?;
    for _ in 0..50 {
        if probe(config).await.is_ok() {
            return Ok(());
        }
        sleep(Duration::from_millis(100)).await;
    }
    anyhow::bail!("Bridge did not recover within 5 seconds; inspect daemon.stderr.log")
}
pub async fn call(config: &Config, request: &Request) -> Result<Reply> {
    ensure_running(config).await?;
    let mut stream = transport::connect(&config.socket()).await?;
    transport::write_frame(&mut stream, request).await?;
    // Do not retry a send after a response timeout: the daemon may have persisted it.
    let reply = timeout(Duration::from_secs(30), transport::read_frame(&mut stream))
        .await
        .context("Bridge response timed out; check status before resending")??;
    if let Reply::Error { message } = &reply {
        anyhow::bail!("{message}");
    }
    Ok(reply)
}
