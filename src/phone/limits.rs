//! Rate limits for the phone listener (REQ-036): token buckets, pure.
//! Time is in milliseconds.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rate {
    /// Requests allowed at once.
    pub burst: f64,
    /// Tokens added per second.
    pub per_second: f64,
}
/// Before authentication, per source address: pairing, challenges, sessions.
pub const UNAUTHENTICATED: Rate = Rate {
    burst: 10.0,
    per_second: 10.0 / 60.0,
};
/// Every call of one phone (polling included).
pub const CALLS: Rate = Rate {
    burst: 20.0,
    per_second: 5.0,
};
/// Chat and channel posts of one phone.
pub const SENDS: Rate = Rate {
    burst: 20.0,
    per_second: 20.0 / 60.0,
};
/// Schedule and role changes of one phone.
pub const WRITES: Rate = Rate {
    burst: 10.0,
    per_second: 10.0 / 60.0,
};
/// The requests a phone makes before it has a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unauthenticated {
    /// Typing a pairing code.
    Pair,
    /// Asking for a sign-in nonce.
    Challenge,
    /// Signing in with that nonce.
    Session,
}
/// Whether a request draws from the address's bucket. A sign-in is a
/// challenge and a session request; only the challenge is charged, so a
/// limited phone never spends its one token on a nonce it then cannot use.
/// A session request needs a fresh, single-use nonce (so it is already paid
/// for) and its failures still count toward `FAILURES`.
pub fn charged(request: Unauthenticated) -> bool {
    match request {
        Unauthenticated::Pair | Unauthenticated::Challenge => true,
        Unauthenticated::Session => false,
    }
}

/// Failed signatures or codes from one address before it must wait.
pub const FAILURES: u32 = 5;
pub const FAILURE_WAIT_MS: u64 = 60_000;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bucket {
    tokens: f64,
    at_ms: u64,
}
impl Bucket {
    pub fn full(rate: Rate, now_ms: u64) -> Self {
        Self {
            tokens: rate.burst,
            at_ms: now_ms,
        }
    }
    /// Takes one token if there is one: the bucket after, and whether the
    /// request may go ahead.
    pub fn take(self, rate: Rate, now_ms: u64) -> (Bucket, bool) {
        let elapsed = now_ms.saturating_sub(self.at_ms) as f64 / 1000.0;
        let tokens = (self.tokens + elapsed * rate.per_second).min(rate.burst);
        let at_ms = now_ms.max(self.at_ms);
        if tokens >= 1.0 {
            (
                Bucket {
                    tokens: tokens - 1.0,
                    at_ms,
                },
                true,
            )
        } else {
            (Bucket { tokens, at_ms }, false)
        }
    }
    /// Seconds until the next token, for `Retry-After`.
    pub fn retry_after(&self, rate: Rate) -> u64 {
        let missing = (1.0 - self.tokens).max(0.0);
        (missing / rate.per_second).ceil().max(1.0) as u64
    }
}

/// Failed attempts of one address: after `FAILURES` in a row it waits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Failures {
    count: u32,
    blocked_until_ms: u64,
}
impl Failures {
    pub fn blocked(&self, now_ms: u64) -> bool {
        now_ms < self.blocked_until_ms
    }
    pub fn fail(self, now_ms: u64) -> Failures {
        let count = self.count + 1;
        if count >= FAILURES {
            Failures {
                count: 0,
                blocked_until_ms: now_ms + FAILURE_WAIT_MS,
            }
        } else {
            Failures { count, ..self }
        }
    }
}

/// Which bucket a phone call draws from besides `CALLS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Read,
    Send,
    Write,
}
pub fn kind(request: &crate::protocol::Request) -> Kind {
    use crate::protocol::Request;
    match request {
        Request::UserSend { .. } | Request::ChannelSend { .. } => Kind::Send,
        Request::SaveSchedule { .. }
        | Request::DeleteSchedule { .. }
        | Request::SaveRole { .. }
        | Request::DeleteRole { .. }
        | Request::AssignRole { .. } => Kind::Write,
        _ => Kind::Read,
    }
}
pub fn rate(kind: Kind) -> Rate {
    match kind {
        Kind::Read => CALLS,
        Kind::Send => SENDS,
        Kind::Write => WRITES,
    }
}
