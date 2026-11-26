//! Remote bridges over SSH (REQ-027). The user's own `ssh` (config, keys,
//! agent) carries one connection per remote host with two forwards: to the
//! remote bridge (`-L`, local port here) and back to this one (`-R`, a port on
//! the remote's loopback). The remote bridge listens on 127.0.0.1 only. Pure;
//! `tunnel` runs and supervises the connections.
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod tunnel;

/// A remote host and the ports of its tunnel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remote {
    /// An SSH destination: a host alias from ~/.ssh/config, a name, or user@host.
    pub host: String,
    /// The remote bridge's port on the remote's loopback.
    pub remote_port: u16,
    /// Where the remote bridge appears on this machine's loopback.
    pub local_port: u16,
    /// Where this bridge appears on the remote's loopback.
    pub back_port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RemoteError {
    #[error(
        "An SSH host is a name, alias or user@host (letters, digits, '.', '_', '-', '@'), not starting with '-'"
    )]
    Host,
}

pub fn validate_host(host: &str) -> Result<(), RemoteError> {
    let plain = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    let ok = !host.starts_with('-')
        && match host.split_once('@') {
            Some((user, name)) => plain(user) && plain(name),
            None => plain(host),
        };
    ok.then_some(()).ok_or(RemoteError::Host)
}

/// The back port on the remote side, stable for this bridge's key so that
/// several bridges sharing one remote host rarely collide.
pub fn back_port(fingerprint: &str) -> u16 {
    let hash = fingerprint.bytes().fold(2_166_136_261u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(16_777_619)
    });
    40000 + (hash % 10000) as u16
}

/// Arguments for `ssh`: no shell, no prompts (keys or agent), fail when a
/// forward cannot be set up, keepalives so a dead link is noticed.
pub fn ssh_args(remote: &Remote, own_port: u16) -> Vec<String> {
    let mut args: Vec<String> = [
        "-N",
        "-o",
        "BatchMode=yes",
        "-o",
        "ExitOnForwardFailure=yes",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=3",
    ]
    .map(String::from)
    .into();
    args.extend([
        "-L".into(),
        format!(
            "127.0.0.1:{}:127.0.0.1:{}",
            remote.local_port, remote.remote_port
        ),
        "-R".into(),
        format!("127.0.0.1:{}:127.0.0.1:{own_port}", remote.back_port),
        "--".into(),
        remote.host.clone(),
    ]);
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote() -> Remote {
        Remote {
            host: "akud@ccbox.example".into(),
            remote_port: 47820,
            local_port: 51001,
            back_port: 45123,
        }
    }

    /* REQ-027: Hosts are plain SSH destinations; nothing that ssh would read as an option. */
    #[test]
    fn validates_hosts() {
        for host in ["ccbox.app", "akud@ccbox.app", "build-box_2", "10.0.0.7"] {
            assert_eq!(validate_host(host), Ok(()), "{host}");
        }
        for host in ["", "-oProxyCommand=sh", "a b", "host;rm", "host\n", "user@"] {
            assert_eq!(validate_host(host), Err(RemoteError::Host), "{host:?}");
        }
    }

    /* REQ-027: One connection forwards to the remote bridge and back to this one, without prompts. */
    #[test]
    fn builds_ssh_arguments() {
        assert_eq!(
            ssh_args(&remote(), 47820),
            [
                "-N",
                "-o",
                "BatchMode=yes",
                "-o",
                "ExitOnForwardFailure=yes",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=3",
                "-L",
                "127.0.0.1:51001:127.0.0.1:47820",
                "-R",
                "127.0.0.1:45123:127.0.0.1:47820",
                "--",
                "akud@ccbox.example",
            ]
        );
    }

    /* REQ-027: The back port is stable per bridge key and stays in the unprivileged range. */
    #[test]
    fn picks_a_stable_back_port() {
        let a = back_port("AAAA");
        assert_eq!(back_port("AAAA"), a);
        assert_ne!(back_port("BBBB"), a);
        for key in ["AAAA", "BBBB", "CCCC", "ZZZZ"] {
            assert!((40000..50000).contains(&back_port(key)), "{key}");
        }
    }
}
