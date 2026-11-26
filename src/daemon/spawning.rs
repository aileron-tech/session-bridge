//! Starting and closing sessions for the user and for root sessions (REQ-028).
use super::{Control, Event};
use crate::{
    discovery::Session,
    protocol::Reply,
    roles,
    spawn::{self, Agent, Caller, SpawnSettings, process, service::Launch},
    store,
};
use anyhow::{Context, Result};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use uuid::Uuid;

/// A terminal window opened and not yet claimed by its launch process.
pub(super) struct Pending {
    agent: Agent,
    cwd: PathBuf,
    name: String,
    prompt: String,
    opened: Instant,
}
const CLAIM_WINDOW: Duration = Duration::from_secs(120);

impl Control {
    /// Local live sessions (parents), as the event loop last saw them.
    async fn local_sessions(&self) -> Result<Vec<Session>> {
        let Reply::Sessions { sessions, .. } = self
            .ask(|reply| Event::List {
                sender: None,
                project: None,
                reply,
            })
            .await??
            .0
        else {
            anyhow::bail!("Could not list the sessions");
        };
        Ok(sessions)
    }

    /// The caller and the session it is, if any.
    async fn caller(&self, sender: Option<&str>, sessions: &[Session]) -> Result<Option<Session>> {
        match sender {
            None => Ok(None),
            Some(sender) => sessions
                .iter()
                .find(|s| s.session_id == sender)
                .cloned()
                .map(Some)
                .context("The calling session is not live on this laptop"),
        }
    }

    pub(super) async fn spawn(
        &self,
        sender: Option<String>,
        project: Option<String>,
        agent: Agent,
        role: String,
    ) -> Result<Reply> {
        let sessions = self.local_sessions().await?;
        let caller = self.caller(sender.as_deref(), &sessions).await?;
        let book = self.roles.lock().await.clone();
        let role = book
            .role(&role)
            .with_context(|| format!("No role named {role}"))?
            .clone();
        // A root session starts in its own directory; the user names a
        // project (by name, where its live sessions share one directory) or
        // a directory.
        let cwd = match (&caller, project) {
            (Some(own), _) => own.cwd.clone(),
            (None, Some(project)) if Path::new(&project).is_absolute() => {
                let path = PathBuf::from(&project);
                anyhow::ensure!(path.is_dir(), "{project} is not a directory");
                path
            }
            (None, Some(project)) => {
                let mut dirs: Vec<&PathBuf> = sessions
                    .iter()
                    .filter(|s| s.project == project)
                    .map(|s| &s.cwd)
                    .collect();
                dirs.sort();
                dirs.dedup();
                match dirs.as_slice() {
                    [one] => (*one).clone(),
                    [] => anyhow::bail!(
                        "No live session in #{project} on this laptop shows its directory; give the directory instead"
                    ),
                    _ => anyhow::bail!(
                        "#{project} is in several directories; give the directory instead"
                    ),
                }
            }
            (None, None) => anyhow::bail!("Name the project or directory to start in"),
        };
        let project = crate::discovery::project_name(&cwd);
        let live = sessions.iter().filter(|s| s.project == project).count();
        let own_role = caller.as_ref().and_then(|c| book.role_of(&c.name));
        let who = match &caller {
            None => Caller::User,
            Some(own) => Caller::Session {
                role: own_role,
                project: &own.project,
            },
        };
        spawn::may_spawn(who, &project, &role.name, live)?;

        let settings = self.spawn_settings.lock().await.clone();
        let short = spawn::short_name(&project, &settings);
        let token = Uuid::new_v4();
        let name = {
            let mut pending = self.pending.lock().await;
            pending.retain(|_, p| p.opened.elapsed() < CLAIM_WINDOW);
            let taken: Vec<&str> = sessions
                .iter()
                .map(|s| s.name.as_str())
                .chain(pending.values().map(|p| p.name.as_str()))
                .collect();
            let name = spawn::next_name(&short, &role.name, taken);
            pending.insert(
                token,
                Pending {
                    agent,
                    cwd,
                    name: name.clone(),
                    prompt: roles::assignment_message(&role),
                    opened: Instant::now(),
                },
            );
            name
        };
        {
            let mut book = self.roles.lock().await;
            let next = book.assign(&name, Some(&role.name))?;
            store::write_json(&self.roles_file, &next)?;
            *book = next;
        }
        let launch = Launch {
            argv: spawn::launch_argv(
                &std::env::current_exe()?,
                token,
                self.launch_state.as_deref(),
            ),
            title: name.clone(),
        };
        let opened = match (self.spawners)(settings.terminal) {
            Ok(spawner) => spawner
                .open(&launch)
                .await
                .with_context(|| format!("Could not open {}", spawner.place())),
            Err(e) => Err(e),
        };
        if let Err(e) = opened {
            self.pending.lock().await.remove(&token);
            let mut book = self.roles.lock().await;
            let next = book.assign(&name, None)?;
            store::write_json(&self.roles_file, &next)?;
            *book = next;
            return Err(e);
        }
        Ok(Reply::Spawned {
            name,
            project,
            agent,
            role: role.name,
        })
    }

    pub(super) async fn claim(&self, token: Uuid, pid: u32) -> Result<Reply> {
        let pending = self
            .pending
            .lock()
            .await
            .remove(&token)
            .filter(|p| p.opened.elapsed() < CLAIM_WINDOW)
            .context("This launch is unknown or expired; start the session again")?;
        self.names
            .lock()
            .map_err(|_| anyhow::anyhow!("Session names are unavailable"))?
            .pids
            .insert(pid, pending.name.clone());
        Ok(Reply::Launch {
            agent: pending.agent,
            cwd: pending.cwd,
            name: pending.name,
            prompt: pending.prompt,
        })
    }

    pub(super) async fn close(&self, sender: Option<String>, session_id: String) -> Result<Reply> {
        let sessions = self.local_sessions().await?;
        let caller = self.caller(sender.as_deref(), &sessions).await?;
        let target = sessions
            .iter()
            .find(|s| s.session_id == session_id)
            .context(
                "That session is not live on this laptop; close a paired laptop's session there",
            )?;
        let book = self.roles.lock().await.clone();
        let who = match &caller {
            None => Caller::User,
            Some(own) => Caller::Session {
                role: book.role_of(&own.name),
                project: &own.project,
            },
        };
        spawn::may_close(who, &target.project, book.role_of(&target.name))?;
        // One process may carry several sessions (a shared Codex app server):
        // ending it would end them all.
        anyhow::ensure!(
            target.pid > 1 && sessions.iter().filter(|s| s.pid == target.pid).count() == 1,
            "{} shares its process with other sessions; close it in its terminal",
            target.name
        );
        let line = process::command_line(target.pid).await?;
        anyhow::ensure!(
            !line.contains("app-server"),
            "{} runs in the shared Codex app server; close it in its terminal",
            target.name
        );
        process::terminate(target.pid).await?;
        Ok(Reply::Closed {
            name: target.name.clone(),
        })
    }

    pub(super) async fn spawn_settings_reply(&self) -> Reply {
        Reply::SpawnSettings {
            settings: self.spawn_settings.lock().await.clone(),
        }
    }

    pub(super) async fn save_spawn_settings(
        &self,
        project: Option<String>,
        short_name: Option<String>,
        terminal: Option<spawn::Terminal>,
    ) -> Result<Reply> {
        let mut settings = self.spawn_settings.lock().await;
        let mut next: SpawnSettings = settings.clone();
        if let Some(project) = project {
            crate::domain::validate_project(&project)?;
            match short_name {
                Some(short) => {
                    spawn::validate_short_name(&short)?;
                    next.short_names.insert(project, short);
                }
                None => {
                    next.short_names.remove(&project);
                }
            }
        }
        if let Some(terminal) = terminal {
            next.terminal = terminal;
        }
        store::write_json(&self.spawn_file, &next)?;
        *settings = next;
        Ok(Reply::SpawnSettings {
            settings: settings.clone(),
        })
    }
}
