//! Windows placeholder: advertising Codex sessions as native Claude peers
//! needs the Claude named-pipe inbox protocol, which is not implemented yet.
use crate::{daemon::Config, discovery::Session};
use anyhow::Result;
use std::path::PathBuf;

#[derive(Default)]
pub struct NativePeers {}
impl NativePeers {
    pub fn socket(&self, _session_id: &str) -> Option<PathBuf> {
        None
    }
    pub async fn sync(&mut self, _config: &Config, _sessions: &[Session]) -> Result<()> {
        Ok(())
    }
    pub async fn shutdown(&mut self) -> Result<()> {
        Ok(())
    }
}
pub async fn serve(_config: Config) -> Result<()> {
    anyhow::bail!("Native Claude peers are not available on Windows yet")
}
