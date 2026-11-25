use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Codex,
    Claude,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionId {
    pub provider: Provider,
    pub id: Uuid,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ValidationError {
    #[error("Expected codex:<uuid> or claude:<uuid>")]
    InvalidSession,
    #[error("Message must be nonempty, at most 16000 bytes, and contain no NUL")]
    InvalidMessage,
    #[error("Project must be a nonempty exact name or absolute directory path, without NUL")]
    InvalidProject,
}

impl SessionId {
    pub fn parse(value: &str) -> Result<Self, ValidationError> {
        let (provider, id) = value
            .split_once(':')
            .ok_or(ValidationError::InvalidSession)?;
        let provider = match provider {
            "codex" => Provider::Codex,
            "claude" => Provider::Claude,
            _ => return Err(ValidationError::InvalidSession),
        };
        let id = Uuid::parse_str(id).map_err(|_| ValidationError::InvalidSession)?;
        Ok(Self { provider, id })
    }
}
impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}",
            match self.provider {
                Provider::Codex => "codex",
                Provider::Claude => "claude",
            },
            self.id
        )
    }
}
pub fn validate_message(message: &str) -> Result<(), ValidationError> {
    if message.trim().is_empty() || message.len() > 16000 || message.contains('\0') {
        return Err(ValidationError::InvalidMessage);
    }
    Ok(())
}
pub fn validate_project(project: &str) -> Result<(), ValidationError> {
    if project.trim().is_empty()
        || project.contains('\0')
        || (project.contains('/') && !std::path::Path::new(project).is_absolute())
    {
        return Err(ValidationError::InvalidProject);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Delivery {
    Pending {
        reason: String,
    },
    Dispatching,
    Queued,
    /// The paired bridge on `host` saved the message and owns its delivery.
    Forwarded {
        host: String,
    },
    Delivered,
    Held {
        reason: String,
    },
    Refused {
        reason: String,
    },
    Uncertain {
        reason: String,
    },
}
pub fn recover_delivery(delivery: &Delivery) -> Delivery {
    match delivery {
        Delivery::Dispatching => Delivery::Uncertain {
            reason: "Service stopped during dispatch; inspect recipient before retrying".into(),
        },
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /* REQ-006: Session addresses preserve provider and reject arbitrary paths. */
    #[test]
    fn resolves_only_explicit_session_addresses() {
        let id = "codex:01a1d465-0b2b-76e2-ac61-2c442c669b71";
        assert_eq!(SessionId::parse(id).unwrap().to_string(), id);
        for bad in [
            "/tmp/a.sock",
            "codex:../x",
            "claude:unknown",
            "other:01a1d465-0b2b-76e2-ac61-2c442c669b71",
        ] {
            assert!(SessionId::parse(bad).is_err());
        }
    }
    /* REQ-006: Invalid content is rejected before persistence or transport. */
    #[test]
    fn rejects_empty_oversized_and_nul_messages() {
        for bad in [
            String::new(),
            "  \n".into(),
            "a\0b".into(),
            "x".repeat(16001),
        ] {
            assert!(validate_message(&bad).is_err());
        }
        assert!(validate_message("Status?\nPlease reply.").is_ok());
    }
    /* REQ-003: Restart preserves unsent work without duplicating uncertain sends. */
    #[test]
    fn recovers_only_interrupted_dispatch_as_uncertain() {
        assert!(matches!(
            recover_delivery(&Delivery::Dispatching),
            Delivery::Uncertain { .. }
        ));
        for state in [
            Delivery::Pending {
                reason: "offline".into(),
            },
            Delivery::Queued,
            Delivery::Held {
                reason: "approval".into(),
            },
        ] {
            assert_eq!(recover_delivery(&state), state);
        }
    }
}
