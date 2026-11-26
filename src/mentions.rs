//! Who a channel post reaches (REQ-020): `@channel` names every session of
//! the project, `@name` one session by its name. Pure.
use thiserror::Error;

/// A session of the channel, as mentions name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub session_id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MentionError {
    #[error("#{0} has no live sessions")]
    EmptyChannel(String),
    #[error("Mention @channel or a session (@name) to post in #{0}")]
    NoMention(String),
    #[error("No session named @{name} in #{channel}")]
    Unknown { name: String, channel: String },
    #[error("Several sessions in #{channel} are named @{name}; message them directly")]
    Ambiguous { name: String, channel: String },
    #[error(
        "The default recipient @{name} is not live in #{channel}; mention someone or change the default"
    )]
    DefaultGone { name: String, channel: String },
}

/// The names mentioned in `text`: `@` at the start or after a character that
/// cannot end a word (so `me@example.com` is not a mention), followed by
/// letters, digits, `.`, `_` or `-`; a trailing `.` or `-` ends the sentence.
pub fn mentioned(text: &str) -> Vec<String> {
    let word = |c: char| c.is_alphanumeric() || matches!(c, '.' | '_' | '-');
    let mut out = Vec::new();
    let mut previous: Option<char> = None;
    for (at, c) in text.char_indices() {
        let starts = c == '@' && !previous.is_some_and(|p| p.is_alphanumeric() || p == '_');
        previous = Some(c);
        if !starts {
            continue;
        }
        let rest = &text[at + 1..];
        let end = rest.find(|c: char| !word(c)).unwrap_or(rest.len());
        let name = rest[..end].trim_end_matches(['.', '-']);
        if !name.is_empty() {
            out.push(name.to_owned());
        }
    }
    out
}

/// Session IDs the post reaches, in member order, without duplicates. A post
/// without a mention goes to the channel's default recipient (REQ-024), if set.
pub fn resolve(
    text: &str,
    members: &[Member],
    channel: &str,
    default: Option<&str>,
) -> Result<Vec<String>, MentionError> {
    if members.is_empty() {
        return Err(MentionError::EmptyChannel(channel.into()));
    }
    let mut names = mentioned(text);
    if names.is_empty() {
        let Some(default) = default else {
            return Err(MentionError::NoMention(channel.into()));
        };
        if !members.iter().any(|m| m.name.eq_ignore_ascii_case(default)) {
            return Err(MentionError::DefaultGone {
                name: default.into(),
                channel: channel.into(),
            });
        }
        names.push(default.into());
    }
    let mut reached = vec![false; members.len()];
    for name in &names {
        if name.eq_ignore_ascii_case("channel") {
            reached.fill(true);
            continue;
        }
        let matches: Vec<usize> = members
            .iter()
            .enumerate()
            .filter(|(_, m)| m.name.eq_ignore_ascii_case(name))
            .map(|(i, _)| i)
            .collect();
        match matches.as_slice() {
            [] => {
                return Err(MentionError::Unknown {
                    name: name.clone(),
                    channel: channel.into(),
                });
            }
            [one] => reached[*one] = true,
            _ => {
                return Err(MentionError::Ambiguous {
                    name: name.clone(),
                    channel: channel.into(),
                });
            }
        }
    }
    Ok(members
        .iter()
        .zip(reached)
        .filter(|(_, r)| *r)
        .map(|(m, _)| m.session_id.clone())
        .collect())
}

#[cfg(test)]
#[path = "mentions_tests.rs"]
mod tests;
