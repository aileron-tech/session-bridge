//! Starting and closing sessions for the user and for root sessions (REQ-028).
use super::{Control, Event};
use crate::{
    discovery::Session,
    projects,
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
    pub(super) async fn local_sessions(&self) -> Result<Vec<Session>> {
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
                // Sessions report resolved directories (/tmp is /private/tmp
                // on macOS), so the path is resolved before it is compared.
                std::fs::canonicalize(&project)
                    .ok()
                    .filter(|p| p.is_dir())
                    .with_context(|| format!("{project} is not a directory"))?
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
        let live = sessions.iter().filter(|s| s.cwd == cwd).count();
        let own_dir = caller
            .as_ref()
            .map(|c| c.cwd.to_string_lossy().into_owned());
        let who = match (&caller, &own_dir) {
            (Some(own), Some(dir)) => Caller::Session {
                role: self.acting_role(&book, own)?,
                project: dir,
            },
            _ => Caller::User,
        };
        spawn::may_spawn(who, &cwd.to_string_lossy(), &role.name, live)?;

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
                    cwd: cwd.clone(),
                    name: name.clone(),
                    prompt: roles::assignment_message(&role),
                    opened: Instant::now(),
                },
            );
            name
        };
        let previous = {
            let mut book = self.roles.lock().await;
            let previous = book.role_of(&name).map(str::to_owned);
            let next = book.assign(&name, Some(&role.name))?;
            store::write_json(&self.roles_file, &next)?;
            *book = next;
            previous
        };
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
            let next = book.assign(&name, previous.as_deref())?;
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

    /// Tells the master of the project in `dir` (other than `subject`) about a
    /// change, as the user's channel post. A failure is logged: the change
    /// itself already happened.
    /// The local sessions the event loop last discovered (no new discovery:
    /// page polls must not run it inside the loop).
    pub(super) fn last_sessions(&self) -> Vec<Session> {
        self.live.borrow().clone()
    }

    pub(super) async fn projects(&self) -> Result<Reply> {
        let sessions = self.last_sessions();
        let book = self.roles.lock().await.clone();
        let chosen: std::collections::BTreeMap<String, String> = self
            .channels
            .lock()
            .await
            .iter()
            .filter_map(|(p, c)| c.master.clone().map(|m| (p.clone(), m)))
            .collect();
        Ok(Reply::Projects {
            projects: projects::projects(&sessions, &book, &chosen),
        })
    }

    pub(super) async fn set_master(&self, project: String, name: Option<String>) -> Result<Reply> {
        crate::domain::validate_project(&project)?;
        anyhow::ensure!(
            !project.contains('/'),
            "A project is named here, not given as a path"
        );
        let name = match name {
            None => None,
            Some(name) => Some(
                self.last_sessions()
                    .into_iter()
                    .find(|s| s.project == project && s.name.eq_ignore_ascii_case(&name))
                    .with_context(|| format!("No live session named {name} in #{project}"))?
                    .name,
            ),
        };
        {
            let mut channels = self.channels.lock().await;
            match name {
                Some(name) => channels.entry(project).or_default().master = Some(name),
                None => {
                    if let Some(settings) = channels.get_mut(&project) {
                        settings.master = None;
                    }
                    channels.retain(|_, s| s != &crate::protocol::ChannelSettings::default());
                }
            }
            store::write_json(&self.channels_file, &*channels)?;
        }
        self.projects().await
    }

    /// The role a calling session acts with: its name's role, except that
    /// root counts only for the session that holds it itself (REQ-028).
    fn acting_role<'a>(
        &self,
        book: &'a crate::roles::RoleBook,
        own: &Session,
    ) -> Result<Option<&'a str>> {
        let names = self
            .names
            .lock()
            .map_err(|_| anyhow::anyhow!("Session names are unavailable"))?;
        Ok(match book.role_of(&own.name) {
            Some(role)
                if spawn::is_root(Some(role))
                    && !spawn::holds_root(book, &names, &own.session_id, &own.name) =>
            {
                None
            }
            role => role,
        })
    }

    /// The claim of a started session must come from its launch: the
    /// connecting process (by the socket's peer credentials, not the request)
    /// is the one it names, and runs exactly `<this executable> launch
    /// <token> [<state>]`. The token alone, visible in the window's command
    /// line, binds nothing. Windows gives no peer process, so it is not
    /// checked there.
    pub(super) async fn verify_claimant(
        &self,
        peer: Option<u32>,
        token: Uuid,
        pid: u32,
    ) -> Result<()> {
        if !cfg!(unix) {
            return Ok(());
        }
        anyhow::ensure!(
            peer == Some(pid),
            "A started session is claimed only by its own launch process"
        );
        let expected = spawn::launch_argv(
            &std::env::current_exe()?,
            token,
            self.launch_state.as_deref(),
        )
        .join(" ");
        anyhow::ensure!(
            process::command_line(pid).await? == expected,
            "Process {pid} is not the launch of this session"
        );
        Ok(())
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
            .insert(
                pid,
                spawn::Launched {
                    name: pending.name.clone(),
                    claimed_at: super::now()?,
                },
            );
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
        let own_dir = caller
            .as_ref()
            .map(|c| c.cwd.to_string_lossy().into_owned());
        let who = match (&caller, &own_dir) {
            (Some(own), Some(dir)) => Caller::Session {
                role: self.acting_role(&book, own)?,
                project: dir,
            },
            _ => Caller::User,
        };
        // A session that really holds root is protected; one that only took
        // a root session's name is not.
        let target_root = {
            let names = self
                .names
                .lock()
                .map_err(|_| anyhow::anyhow!("Session names are unavailable"))?;
            spawn::holds_root(&book, &names, &target.session_id, &target.name)
        };
        spawn::may_close(
            who,
            &target.cwd.to_string_lossy(),
            target_root.then_some(spawn::ROOT),
        )?;
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
