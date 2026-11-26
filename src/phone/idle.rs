//! A connection that ends when it neither reads nor writes for a while
//! (REQ-036), so kept-alive phone connections cannot hold every slot.
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    time::{Instant, Sleep, sleep},
};

/// Longer than any phone request takes, short enough to free idle slots.
pub const IDLE: Duration = Duration::from_secs(60);

pub struct Idle<S> {
    inner: S,
    limit: Duration,
    deadline: Pin<Box<Sleep>>,
}
impl<S> Idle<S> {
    pub fn new(inner: S, limit: Duration) -> Self {
        Self {
            inner,
            limit,
            deadline: Box::pin(sleep(limit)),
        }
    }
    fn touch(&mut self) {
        let next = Instant::now() + self.limit;
        self.deadline.as_mut().reset(next);
    }
    /// After the inner stream is pending: has the deadline passed?
    fn expired(&mut self, cx: &mut Context<'_>) -> bool {
        self.deadline.as_mut().poll(cx).is_ready()
    }
}
fn timed_out() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::TimedOut, "Phone connection idle")
}
impl<S: AsyncRead + Unpin> AsyncRead for Idle<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match Pin::new(&mut self.inner).poll_read(cx, buf) {
            Poll::Ready(result) => {
                self.touch();
                Poll::Ready(result)
            }
            Poll::Pending if self.expired(cx) => Poll::Ready(Err(timed_out())),
            Poll::Pending => Poll::Pending,
        }
    }
}
impl<S: AsyncWrite + Unpin> AsyncWrite for Idle<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match Pin::new(&mut self.inner).poll_write(cx, buf) {
            Poll::Ready(result) => {
                self.touch();
                Poll::Ready(result)
            }
            Poll::Pending if self.expired(cx) => Poll::Ready(Err(timed_out())),
            Poll::Pending => Poll::Pending,
        }
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};

    /* REQ-036: A phone connection that stays idle past its limit ends with a timeout. */
    #[tokio::test]
    async fn idle_connection_times_out() {
        let (_peer, ours) = duplex(64);
        let mut idle = Idle::new(ours, Duration::from_millis(100));
        let started = std::time::Instant::now();
        let mut buf = [0u8; 8];
        let error = idle.read(&mut buf).await.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() >= Duration::from_millis(100));
    }

    /* REQ-036: Traffic keeps a phone connection open past its idle limit. */
    #[tokio::test]
    async fn traffic_keeps_the_connection() {
        let (mut peer, ours) = duplex(64);
        let mut idle = Idle::new(ours, Duration::from_millis(150));
        let writer = tokio::spawn(async move {
            for _ in 0..4 {
                sleep(Duration::from_millis(80)).await;
                peer.write_all(b"x").await.unwrap();
            }
            peer
        });
        let mut buf = [0u8; 1];
        for _ in 0..4 {
            assert_eq!(idle.read(&mut buf).await.unwrap(), 1);
        }
        let _peer = writer.await.unwrap();
    }
}
