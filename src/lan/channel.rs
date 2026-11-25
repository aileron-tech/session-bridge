//! Encrypted, mutually authenticated channel between two bridges:
//! Noise_XX_25519_ChaChaPoly_BLAKE2s over any byte stream. Each Noise message
//! travels with a 2-byte length; an application frame is a 4-byte length plus
//! JSON, split over as many Noise messages as needed.
use super::model::Fingerprint;
use crate::transport::FRAME_LIMIT;
use anyhow::{Context, Result};
use serde::{Serialize, de::DeserializeOwned};
use snow::{Builder, HandshakeState, TransportState};
use std::time::Duration;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    time::timeout,
};

const PARAMS: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
const PROLOGUE: &[u8] = b"session-bridge lan v1";
const NOISE_MAX: usize = 65535;
const TAG: usize = 16;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// A bridge's static X25519 key pair; its public half is the bridge identity.
#[derive(Clone)]
pub struct StaticKey {
    private: Vec<u8>,
    public: Vec<u8>,
}
impl StaticKey {
    pub fn generate() -> Result<Self> {
        let pair = builder()?.generate_keypair()?;
        Ok(Self {
            private: pair.private.to_vec(),
            public: pair.public.to_vec(),
        })
    }
    pub fn from_parts(private: Vec<u8>, public: Vec<u8>) -> Result<Self> {
        anyhow::ensure!(
            private.len() == 32 && public.len() == 32,
            "A bridge key has 32-byte halves"
        );
        Ok(Self { private, public })
    }
    pub fn private(&self) -> &[u8] {
        &self.private
    }
    pub fn public(&self) -> &[u8] {
        &self.public
    }
    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint::of_public_key(&self.public)
    }
}

fn builder() -> Result<Builder<'static>> {
    Ok(Builder::new(PARAMS.parse()?))
}

pub struct Channel<S> {
    stream: S,
    noise: TransportState,
    remote: Fingerprint,
}
impl<S: AsyncRead + AsyncWrite + Unpin> Channel<S> {
    /// Runs the initiator side of the handshake.
    pub async fn initiate(stream: S, key: &StaticKey) -> Result<Self> {
        let state = builder()?
            .local_private_key(&key.private)?
            .prologue(PROLOGUE)?
            .build_initiator()?;
        timeout(HANDSHAKE_TIMEOUT, handshake(stream, state, true))
            .await
            .context("Bridge handshake timed out")?
    }
    /// Runs the responder side of the handshake.
    pub async fn respond(stream: S, key: &StaticKey) -> Result<Self> {
        let state = builder()?
            .local_private_key(&key.private)?
            .prologue(PROLOGUE)?
            .build_responder()?;
        timeout(HANDSHAKE_TIMEOUT, handshake(stream, state, false))
            .await
            .context("Bridge handshake timed out")?
    }
    /// The verified key fingerprint of the other bridge.
    pub fn remote(&self) -> &Fingerprint {
        &self.remote
    }
    pub async fn send<T: Serialize>(&mut self, value: &T) -> Result<()> {
        let body = serde_json::to_vec(value)?;
        anyhow::ensure!(
            body.len() as u64 <= FRAME_LIMIT,
            "Message frame exceeds limit"
        );
        let mut plain = (body.len() as u32).to_be_bytes().to_vec();
        plain.extend_from_slice(&body);
        let mut cipher = vec![0u8; NOISE_MAX];
        for chunk in plain.chunks(NOISE_MAX - TAG) {
            let n = self.noise.write_message(chunk, &mut cipher)?;
            timeout(WRITE_TIMEOUT, write_message(&mut self.stream, &cipher[..n]))
                .await
                .context("Bridge write timed out")??;
        }
        Ok(())
    }
    /// Receives one frame; fails when `wait` passes first.
    pub async fn recv<T: DeserializeOwned>(&mut self, wait: Duration) -> Result<T> {
        let body = timeout(wait, self.recv_frame())
            .await
            .context("Bridge receive timed out")??;
        Ok(serde_json::from_slice(&body)?)
    }
    async fn recv_frame(&mut self) -> Result<Vec<u8>> {
        let mut plain = Vec::new();
        let mut buffer = vec![0u8; NOISE_MAX];
        loop {
            let message = read_message(&mut self.stream).await?;
            let n = self.noise.read_message(&message, &mut buffer)?;
            plain.extend_from_slice(&buffer[..n]);
            if plain.len() < 4 {
                continue;
            }
            let total = u32::from_be_bytes([plain[0], plain[1], plain[2], plain[3]]) as usize;
            anyhow::ensure!(
                total as u64 <= FRAME_LIMIT,
                "Invalid or oversized message frame"
            );
            if plain.len() >= total + 4 {
                anyhow::ensure!(plain.len() == total + 4, "Trailing data after frame");
                return Ok(plain.split_off(4));
            }
        }
    }
}

async fn handshake<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    mut state: HandshakeState,
    initiator: bool,
) -> Result<Channel<S>> {
    let mut buffer = vec![0u8; NOISE_MAX];
    // XX: -> e; <- e, ee, s, es; -> s, se.
    for step in 0..3 {
        let writing = (step % 2 == 0) == initiator;
        if writing {
            let n = state.write_message(&[], &mut buffer)?;
            write_message(&mut stream, &buffer[..n]).await?;
        } else {
            let message = read_message(&mut stream).await?;
            state.read_message(&message, &mut buffer)?;
        }
    }
    let remote = Fingerprint::of_public_key(
        state
            .get_remote_static()
            .context("Peer sent no static key")?,
    );
    Ok(Channel {
        stream,
        noise: state.into_transport_mode()?,
        remote,
    })
}

async fn write_message<S: AsyncWrite + Unpin>(stream: &mut S, message: &[u8]) -> Result<()> {
    let length = u16::try_from(message.len()).context("Noise message too long")?;
    let mut data = length.to_be_bytes().to_vec();
    data.extend_from_slice(message);
    stream.write_all(&data).await?;
    stream.flush().await?;
    Ok(())
}
async fn read_message<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Vec<u8>> {
    let mut length = [0u8; 2];
    stream
        .read_exact(&mut length)
        .await
        .context("Bridge connection closed")?;
    let mut message = vec![0u8; u16::from_be_bytes(length) as usize];
    stream
        .read_exact(&mut message)
        .await
        .context("Bridge connection closed mid-message")?;
    Ok(message)
}
