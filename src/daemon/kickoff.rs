//! Scheduled runs that never stack a post on an unread one (REQ-025), and
//! the team table a kickoff carries (REQ-031).
use super::{Control, Event, Target, now};
use crate::{
    discovery::Session,
    domain::{self, SessionId},
    mentions::Member,
    protocol::{LastRunView, MessageView, Reply, Request},
    schedule::runs::{self, RunPost, ScheduleRun, Unread},
    store,
    team::{self, LastMessage, TeamRow},
    usage,
};
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use uuid::Uuid;

/// The message size limit a team table must fit in.
const MESSAGE_LIMIT: usize = 16_000;

impl Control {
    async fn recent_messages(&self) -> Result<Vec<MessageView>> {
        match self
            .ask(|reply| Event::Rpc(Request::Messages, reply))
            .await?
        {
            Reply::Messages { messages, .. } => Ok(messages),
            other => anyhow::bail!("Could not read messages: {other:?}"),
        }
    }

    pub(super) async fn last_runs(&self) -> BTreeMap<Uuid, LastRunView> {
        self.schedule_runs
            .lock()
            .await
            .iter()
            .filter_map(|(id, state)| {
                state.last_run.as_ref().map(|run| {
                    (
                        *id,
                        LastRunView {
                            at: run.at,
                            summary: runs::summary(run),
                        },
                    )
                })
            })
            .collect()
    }

    /// Whether this schedule's previous post to `member` is still unread.
    async fn still_unread(
        &self,
        previous: &RunPost,
        local: bool,
        messages: &[MessageView],
    ) -> bool {
        let Some(view) = messages
            .iter()
            .find(|m| m.message_id == previous.message_id)
        else {
            return false;
        };
        match runs::unread(&view.delivery, &view.recipient.provider, local) {
            Unread::Yes => true,
            Unread::No => false,
            Unread::IfInCodexQueue => {
                match crate::codex_queue::waiting(&self.codex_home, &view.recipient.id).await {
                    Ok(items) => items
                        .iter()
                        .any(|item| item.message_id == Some(previous.message_id)),
                    Err(e) => {
                        eprintln!("Cannot read Codex's queue; posting anyway: {e:#}");
                        false
                    }
                }
            }
        }
    }

    /// Runs a schedule: posts to each recipient whose previous post from this
    /// schedule was taken, skips and reports the others.
    pub(super) async fn run_schedule(&self, id: Uuid) -> Result<Reply> {
        let schedule = self
            .schedules
            .lock()
            .await
            .iter()
            .find(|s| s.id == id)
            .cloned()
            .with_context(|| format!("No schedule {id}; it was deleted on the admin page"))?;
        let target = match schedule.to.as_deref() {
            None => Target::Mentions,
            Some(to) if to.eq_ignore_ascii_case("channel") => Target::Channel,
            Some(name) => Target::Session(name.to_owned()),
        };
        let reached = self
            .recipients(&schedule.project, &schedule.message, &target)
            .await?;
        let state = self
            .schedule_runs
            .lock()
            .await
            .get(&id)
            .cloned()
            .unwrap_or_default();
        let messages = self.recent_messages().await?;
        // Sessions on paired laptops, read only when a previous post needs checking.
        let remote: Vec<String> = if reached
            .iter()
            .any(|m| state.posts.contains_key(&m.session_id))
        {
            match self.list(None, Some(schedule.project.clone())).await? {
                Reply::Sessions { sessions, .. } => sessions
                    .into_iter()
                    .filter(|s| s.host.is_some())
                    .map(|s| s.session_id)
                    .collect(),
                _ => Vec::new(),
            }
        } else {
            Vec::new()
        };
        let at = now()?;
        let (mut send, mut skipped) = (Vec::new(), Vec::new());
        for member in reached {
            let local = !remote.contains(&member.session_id);
            match state.posts.get(&member.session_id) {
                Some(previous) if self.still_unread(previous, local, &messages).await => {
                    skipped.push(runs::skip_line(&member.name, previous.at, at));
                }
                _ => send.push(member),
            }
        }
        let message = if schedule.team_report && !send.is_empty() {
            self.with_team(&schedule.project, &schedule.message, &messages)
                .await?
        } else {
            schedule.message.clone()
        };
        let (receipts, failed) = if send.is_empty() {
            (Vec::new(), Vec::new())
        } else {
            self.send_post(&schedule.project, &message, &send, true)
                .await?
        };
        let mut next = state;
        for receipt in &receipts {
            next.posts.insert(
                receipt.recipient.to_string(),
                RunPost {
                    message_id: receipt.message_id,
                    at,
                },
            );
        }
        next.last_run = Some(ScheduleRun {
            at,
            posted: send
                .iter()
                .filter(|m| {
                    receipts
                        .iter()
                        .any(|r| r.recipient.to_string() == m.session_id)
                })
                .map(|m| m.name.clone())
                .collect(),
            skipped: skipped.clone(),
            failed: failed.clone(),
        });
        {
            let mut all = self.schedule_runs.lock().await;
            all.insert(id, next);
            store::write_json(&self.schedule_runs_file, &*all)?;
        }
        Ok(Reply::Posted {
            project: schedule.project,
            receipts,
            failed,
            skipped,
        })
    }

    /// A direct message from the user with the team table of the session's project.
    pub(super) async fn send_with_team(
        &self,
        session_id: String,
        message: String,
        channel: Option<String>,
        scheduled: bool,
    ) -> Result<Reply> {
        domain::validate_message(&message)?;
        let Reply::Sessions { sessions, .. } = self.list(None, None).await? else {
            anyhow::bail!("Could not list the sessions");
        };
        let project = sessions
            .iter()
            .find(|s| s.session_id == session_id)
            .map(|s| s.project.clone())
            .context("That session is not live; the team table needs its project")?;
        let messages = self.recent_messages().await?;
        let message = self.with_team(&project, &message, &messages).await?;
        let request = Request::UserSend {
            session_id,
            message,
            channel,
            scheduled,
            team_report: false,
        };
        self.ask(|reply| Event::Rpc(request, reply)).await
    }

    /// `message` with the team table of #`project`, for a post the user writes now.
    pub(super) async fn with_team_now(&self, project: &str, message: &str) -> Result<String> {
        domain::validate_message(message)?;
        let messages = self.recent_messages().await?;
        self.with_team(project, message, &messages).await
    }

    /// `message` with the table of #`project`'s sessions at this moment.
    async fn with_team(
        &self,
        project: &str,
        message: &str,
        messages: &[MessageView],
    ) -> Result<String> {
        let Reply::Sessions { sessions, .. } = self.list(None, Some(project.to_owned())).await?
        else {
            anyhow::bail!("Could not list the sessions of #{project}");
        };
        let sessions: Vec<&Session> = sessions
            .iter()
            .filter(|s| s.agent != domain::Provider::User)
            .collect();
        let local: Vec<(String, u32)> = sessions
            .iter()
            .filter(|s| s.host.is_none() && s.pid > 0)
            .map(|s| (s.session_id.clone(), s.pid))
            .collect();
        let usage: BTreeMap<String, String> = match self.usage.processes().await {
            Ok(table) => usage::session_usage(&local, &table)
                .into_iter()
                .map(|v| (v.session_id, v.summary))
                .collect(),
            Err(e) => {
                eprintln!("Team table without usage: {e:#}");
                BTreeMap::new()
            }
        };
        let members: Vec<Member> = self
            .members(&sessions.iter().map(|s| (*s).clone()).collect::<Vec<_>>())
            .await;
        let rows: Vec<TeamRow> = sessions
            .iter()
            .map(|s| TeamRow {
                name: s.name.clone(),
                role: members
                    .iter()
                    .find(|m| m.session_id == s.session_id)
                    .and_then(|m| m.role.clone()),
                agent: agent_name(&s.agent).to_owned(),
                host: s.host.clone().unwrap_or_else(|| "this laptop".into()),
                status: s.status.clone(),
                usage: usage.get(&s.session_id).cloned(),
                last: last_message(&s.session_id, messages),
            })
            .collect();
        Ok(team::attach(message, project, &rows, now()?, MESSAGE_LIMIT))
    }
}

fn agent_name(agent: &domain::Provider) -> &'static str {
    match agent {
        domain::Provider::Codex => "codex",
        domain::Provider::Claude => "claude",
        domain::Provider::User => "user",
    }
}

/// When `session` last sent or received a message (`messages` is newest first).
fn last_message(session: &str, messages: &[MessageView]) -> Option<LastMessage> {
    let is = |id: &SessionId| id.to_string() == session;
    messages
        .iter()
        .find(|m| is(&m.sender) || is(&m.recipient))
        .map(|m| LastMessage {
            at: m.created_at,
            sent: is(&m.sender),
        })
}
