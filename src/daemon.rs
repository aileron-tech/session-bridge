use crate::{
    discovery::{self, Session, Sources},
    domain::{self, Delivery, Provider, SessionId},
    lan::{
        model::{ForwardOutcome, PairedPeer, apply_remote_status, complete_forward},
        service::{Inbound, Lan, PairStep},
        wire::{ForwardedMessage, PeerReply, RemoteSender},
    },
    mentions::{self, Member},
    platform,
    protocol::{ChannelSettings, MessageView, RemoteView, Reply, Request, ScheduleView},
    remote::{self, Remote, tunnel::Tunnels},
    roles::{self, Role, RoleBook},
    schedule::{self, Schedule},
    spawn::{
        SessionNames, SpawnSettings, Terminal,
        service::{self, Spawner},
    },
    store::{self, Envelope, Route, Store},
    transport::{self, PeerFrame},
    usage::sampler::Sampler,
};
use anyhow::{Context, Result};
use fs2::FileExt;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{Mutex as AsyncMutex, mpsc, oneshot, watch},
    task::JoinSet,
};
use uuid::Uuid;

mod kickoff;
mod spawning;
mod usage;
mod waiting;
mod wake;

#[derive(Debug, Clone)]
pub struct Config {
    pub state: PathBuf,
    /// The user's home: its filesystem is the disk the usage shows (REQ-029).
    pub home: PathBuf,
    pub sources: Sources,
    pub codex: PathBuf,
    /// Desktop notifications for pairing requests.
    pub notify: bool,
    /// The installed service (LaunchAgent, logon task) owns this state
    /// directory, so clients may ask it to start. False for a bridge run from
    /// a shell on its own state directory (REQ-021).
    pub supervised: bool,
    /// Show the macOS menu-bar icon while the bridge runs (REQ-022).
    pub tray: bool,
    /// Where the latest release is described (REQ-023).
    pub releases: String,
    /// The crontab program (REQ-025); tests use a stand-in.
    pub crontab: String,
    /// The ssh program for remote hosts (REQ-027); tests use a stand-in.
    pub ssh: String,
    /// A program that stands in for the terminal window of a started
    /// session (REQ-028); tests use one. `None` opens a real terminal.
    pub terminal: Option<String>,
}

/// Who a channel post reaches.
enum Target {
    /// The mentions in the text, or the channel's default recipient.
    Mentions,
    Channel,
    Session(String),
}
impl Config {
    pub fn socket(&self) -> PathBuf {
        platform::control_endpoint(&self.state)
    }
}
type ListResult = Result<(Reply, Option<String>)>;
enum Event {
    Rpc(Request, oneshot::Sender<Reply>),
    Peer(Route, Box<PeerFrame>),
    /// Local listing and the project scope it resolved.
    List {
        sender: Option<String>,
        project: Option<String>,
        reply: oneshot::Sender<ListResult>,
    },
    RemoteSeen(Vec<Session>),
    /// Leave the event loop and stop cleanly.
    Shutdown,
    Forwarded {
        id: Uuid,
        outcome: ForwardOutcome,
    },
    /// The user's posts that Codex queued and has not run yet, as
    /// (thread, message id), over every message (REQ-015).
    QueuedUserPosts(oneshot::Sender<Vec<(Uuid, Uuid)>>),
}

/// Entry point for control requests from the CLI, MCP adapters and the admin
/// page: LAN setup runs here, message work goes to the daemon's event loop.
#[derive(Clone)]
pub struct Control {
    events: mpsc::Sender<Event>,
    lan: Lan,
    admin: Arc<AsyncMutex<Option<crate::admin::Admin>>>,
    /// Channel settings, saved in `channels.json` (REQ-024).
    channels: Arc<AsyncMutex<BTreeMap<String, ChannelSettings>>>,
    channels_file: PathBuf,
    /// Scheduled posts, saved in `schedules.json` and run by cron (REQ-025).
    schedules: Arc<AsyncMutex<Vec<Schedule>>>,
    schedules_file: PathBuf,
    /// Each schedule's last run and last post per session (REQ-025).
    schedule_runs: Arc<AsyncMutex<BTreeMap<Uuid, schedule::runs::ScheduleState>>>,
    schedule_runs_file: PathBuf,
    /// Roles and their assignments, saved in `roles.json` (REQ-026).
    roles: Arc<AsyncMutex<RoleBook>>,
    roles_file: PathBuf,
    crontab: String,
    /// The command cron runs: this executable, and a separate state directory.
    cron_command: String,
    /// Remote hosts reached over SSH, saved in `remotes.json` (REQ-027).
    remotes: Arc<AsyncMutex<Vec<Remote>>>,
    remotes_file: PathBuf,
    tunnels: Tunnels,
    /// Short project names and the terminal, saved in `spawn.json` (REQ-028).
    spawn_settings: Arc<AsyncMutex<SpawnSettings>>,
    spawn_file: PathBuf,
    /// Terminal windows opened and not yet claimed, by launch token.
    pending: Arc<AsyncMutex<BTreeMap<Uuid, spawning::Pending>>>,
    /// Names of started sessions, shared with the event loop.
    names: Arc<std::sync::Mutex<SessionNames>>,
    /// The spawn service for the terminal the user chose (REQ-028).
    spawners: Arc<dyn Fn(Terminal) -> Result<Box<dyn Spawner>> + Send + Sync>,
    /// The state directory a launch names: set for a bridge run on its own.
    launch_state: Option<PathBuf>,
    /// Codex's home, whose queue shows what waits for a Codex thread (REQ-032).
    codex_home: PathBuf,
    /// Reads host and process usage (REQ-029), shared with the LAN service.
    usage: Sampler,
    /// The last discovered local sessions, published by the event loop.
    live: watch::Receiver<Vec<Session>>,
}
impl Control {
    /// A request from the control socket from the process `peer` (its peer
    /// credentials, where the system gives them).
    pub async fn call_from(&self, peer: Option<u32>, request: Request) -> Reply {
        if let Request::ClaimSpawn { token, pid } = &request
            && let Err(e) = self.verify_claimant(peer, *token, *pid).await
        {
            return Reply::Error {
                message: format!("{e:#}"),
            };
        }
        self.call(request).await
    }
    /// A request from the control socket (CLI, MCP adapters): it cannot write as the user.
    pub async fn call(&self, request: Request) -> Reply {
        if matches!(
            request,
            Request::UserSend { .. }
                | Request::ChannelSend { .. }
                | Request::ChannelDefault { .. }
                | Request::SaveSchedule { .. }
                | Request::DeleteSchedule { .. }
                | Request::SaveRole { .. }
                | Request::DeleteRole { .. }
                | Request::AssignRole { .. }
                | Request::SaveSpawnSettings { .. }
                | Request::ProjectMaster { .. }
        ) {
            return Reply::Error {
                message: "Only the admin page sends messages as the user".into(),
            };
        }
        self.reply(request).await
    }
    /// A request from the admin page, which checked its token and allowlist.
    /// The page starts and closes sessions as the user, never as a session.
    pub async fn call_admin(&self, request: Request) -> Reply {
        if matches!(
            request,
            Request::Spawn {
                sender: Some(_),
                ..
            } | Request::Close {
                sender: Some(_),
                ..
            }
        ) {
            return Reply::Error {
                message: "The admin page starts and closes sessions as the user".into(),
            };
        }
        self.reply(request).await
    }
    async fn reply(&self, request: Request) -> Reply {
        self.answer(request).await.unwrap_or_else(|e| Reply::Error {
            message: format!("{e:#}"),
        })
    }
    async fn answer(&self, request: Request) -> Result<Reply> {
        Ok(match request {
            Request::Network {
                enabled,
                port,
                name,
                local_only,
            } => {
                let lan = self
                    .lan
                    .set_network(enabled, port, name, local_only)
                    .await?;
                self.sync_tunnels().await?;
                Reply::Lan { lan }
            }
            Request::Remotes => self.remotes().await?,
            Request::AddRemote { host, port } => {
                remote::validate_host(&host)?;
                let lan = self.lan.view().await?;
                anyhow::ensure!(
                    lan.network.port.is_some(),
                    "Turn networking on first: session-bridge network on (add --local-only to stay off the local network)"
                );
                let mut remotes = self.remotes.lock().await;
                anyhow::ensure!(
                    !remotes.iter().any(|r| r.host == host),
                    "{host} is already a remote host"
                );
                let local_port = std::net::TcpListener::bind(("127.0.0.1", 0))?
                    .local_addr()?
                    .port();
                let mut next = remotes.clone();
                next.push(Remote {
                    host,
                    remote_port: port.unwrap_or(crate::lan::state::DEFAULT_PORT),
                    local_port,
                    back_port: remote::back_port(&lan.network.fingerprint),
                });
                store::write_json(&self.remotes_file, &next)?;
                *remotes = next;
                drop(remotes);
                self.sync_tunnels().await?;
                self.remotes().await?
            }
            Request::RemoveRemote { host } => {
                let mut remotes = self.remotes.lock().await;
                let next: Vec<Remote> =
                    remotes.iter().filter(|r| r.host != host).cloned().collect();
                anyhow::ensure!(next.len() < remotes.len(), "{host} is not a remote host");
                store::write_json(&self.remotes_file, &next)?;
                *remotes = next;
                drop(remotes);
                self.tunnels.remove(&host).await;
                self.remotes().await?
            }
            Request::Lan => Reply::Lan {
                lan: self.lan_view().await?,
            },
            Request::Pair { host } => {
                let through = self
                    .remotes
                    .lock()
                    .await
                    .iter()
                    .find(|r| r.host == host)
                    .cloned();
                let pairing = match through {
                    Some(remote) => {
                        let address =
                            std::net::SocketAddr::from(([127, 0, 0, 1], remote.local_port));
                        self.lan
                            .pair_through(&host, address, remote.back_port)
                            .await?
                    }
                    None => self.lan.pair(&host).await?,
                };
                Reply::Pairing {
                    id: pairing.id,
                    host: pairing.host,
                    expires_in: pairing.expires_in,
                }
            }
            Request::PairCode { id, code } => match self.lan.submit_code(id, &code).await? {
                PairStep::Paired { host, key } => Reply::Paired { host, key },
                PairStep::WrongCode { attempts_left } => Reply::WrongCode { attempts_left },
            },
            Request::Unpair { host } => Reply::Unpaired {
                host: self.lan.unpair(&host)?,
            },
            Request::Admin => {
                let mut admin = self.admin.lock().await;
                if admin.is_none() {
                    *admin = Some(crate::admin::start(self.clone())?);
                }
                Reply::Admin {
                    url: admin.as_ref().context("Admin page missing")?.url.clone(),
                }
            }
            Request::List { sender, project } => self.list(sender, project).await?,
            Request::ChannelSend { project, message } => {
                self.post(project, message, Target::Mentions, false).await?
            }
            Request::Schedules => self.schedules().await?,
            Request::SaveSchedule {
                id,
                cron,
                project,
                to,
                message,
                team_report,
            } => {
                let schedule = Schedule {
                    id: id.unwrap_or_else(Uuid::new_v4),
                    cron: cron.trim().to_owned(),
                    project,
                    to: to.filter(|t| !t.is_empty()),
                    message,
                    team_report,
                };
                schedule::validate(&schedule)?;
                let mut schedules = self.schedules.lock().await;
                let mut next = schedules.clone();
                match next.iter_mut().find(|s| s.id == schedule.id) {
                    Some(existing) => *existing = schedule,
                    None => next.push(schedule),
                }
                self.store_schedules(&schedules, &next).await?;
                *schedules = next;
                drop(schedules);
                self.schedules().await?
            }
            Request::DeleteSchedule { id } => {
                let mut schedules = self.schedules.lock().await;
                let next: Vec<Schedule> =
                    schedules.iter().filter(|s| s.id != id).cloned().collect();
                anyhow::ensure!(next.len() < schedules.len(), "No schedule {id}");
                self.store_schedules(&schedules, &next).await?;
                *schedules = next;
                drop(schedules);
                self.schedules().await?
            }
            Request::Roles => {
                let book = self.roles.lock().await.clone();
                Reply::Roles {
                    roles: book.roles,
                    assignments: book.assignments,
                }
            }
            Request::SaveRole {
                name,
                prompt,
                previous,
            } => {
                let mut book = self.roles.lock().await;
                let next = book.save(Role { name, prompt }, previous.as_deref())?;
                store::write_json(&self.roles_file, &next)?;
                *book = next;
                Reply::Roles {
                    roles: book.roles.clone(),
                    assignments: book.assignments.clone(),
                }
            }
            Request::DeleteRole { name } => {
                let mut book = self.roles.lock().await;
                let next = book.delete(&name)?;
                store::write_json(&self.roles_file, &next)?;
                *book = next;
                Reply::Roles {
                    roles: book.roles.clone(),
                    assignments: book.assignments.clone(),
                }
            }
            Request::AssignRole { session_id, role } => {
                let Reply::Sessions { sessions, .. } = self.list(None, None).await? else {
                    anyhow::bail!("Could not list the sessions");
                };
                let session = sessions
                    .iter()
                    .find(|s| s.session_id == session_id)
                    .context("That session is not live")?;
                let (next, prompt) = {
                    let book = self.roles.lock().await;
                    let next =
                        book.assign_session(&session.session_id, &session.name, role.as_deref())?;
                    let prompt = role
                        .as_deref()
                        .and_then(|r| book.role(r))
                        .map(roles::assignment_message);
                    (next, prompt)
                };
                store::write_json(&self.roles_file, &next)?;
                *self.roles.lock().await = next.clone();
                if let Some(message) = prompt {
                    let request = Request::UserSend {
                        session_id,
                        message,
                        channel: None,
                        scheduled: false,
                        team_report: false,
                    };
                    if let Reply::Error { message } =
                        self.ask(|reply| Event::Rpc(request, reply)).await?
                    {
                        anyhow::bail!("Role saved, but its prompt was not sent: {message}");
                    }
                }
                Reply::Roles {
                    roles: next.roles,
                    assignments: next.assignments,
                }
            }
            Request::RunSchedule { id } => self.run_schedule(id).await?,
            Request::UserSend {
                session_id,
                message,
                channel,
                scheduled,
                team_report: true,
            } => {
                self.send_with_team(session_id, message, channel, scheduled)
                    .await?
            }
            Request::Channels => Reply::Channels {
                channels: self.channels.lock().await.clone(),
            },
            Request::ChannelDefault { project, name } => self.set_default(project, name).await?,
            Request::Spawn {
                sender,
                project,
                agent,
                role,
            } => self.spawn(sender, project, agent, role).await?,
            Request::Close { sender, session_id } => self.close(sender, session_id).await?,
            Request::ClaimSpawn { token, pid } => self.claim(token, pid).await?,
            Request::SpawnSettings => self.spawn_settings_reply().await,
            Request::Projects => self.projects().await?,
            Request::Waiting => self.waiting().await?,
            Request::ProjectMaster { project, name } => self.set_master(project, name).await?,
            Request::Usage => self.usage().await?,
            Request::SaveSpawnSettings {
                project,
                short_name,
                terminal,
            } => {
                self.save_spawn_settings(project, short_name, terminal)
                    .await?
            }
            Request::Shutdown => {
                self.events.send(Event::Shutdown).await?;
                Reply::ShuttingDown {
                    pid: std::process::id(),
                }
            }
            other => self.ask(|reply| Event::Rpc(other, reply)).await?,
        })
    }
    /// Sessions here and on paired laptops, in the resolved scope.
    async fn list(&self, sender: Option<String>, project: Option<String>) -> Result<Reply> {
        Ok({
            let (reply, scope) = self
                .ask(|reply| Event::List {
                    sender,
                    project,
                    reply,
                })
                .await??;
            let Reply::Sessions {
                mut sessions,
                recent_messages,
                ..
            } = reply
            else {
                return Ok(reply);
            };
            let (remote, unavailable_hosts) = self.lan.list_remote(scope).await?;
            let fresh: Vec<Session> = remote
                .into_iter()
                .filter(|r| !sessions.iter().any(|s| s.session_id == r.session_id))
                .collect();
            self.events.send(Event::RemoteSeen(fresh.clone())).await?;
            sessions.extend(fresh);
            Reply::Sessions {
                sessions,
                recent_messages,
                unavailable_hosts,
            }
        })
    }
    /// Posts in a project channel to the sessions the text mentions (REQ-020).
    /// Sets (by a live session's name) or clears a channel's default recipient.
    async fn set_default(&self, project: String, name: Option<String>) -> Result<Reply> {
        domain::validate_project(&project)?;
        anyhow::ensure!(
            !project.contains('/'),
            "A channel is a project name, not a path"
        );
        let name = match name {
            None => None,
            Some(name) => {
                let Reply::Sessions { sessions, .. } =
                    self.list(None, Some(project.clone())).await?
                else {
                    anyhow::bail!("Could not list the sessions of #{project}");
                };
                let members = self.members(&sessions).await;
                mentions::resolve(&format!("@{name}"), &members, &project, None)?;
                // A session name, or else a role (REQ-026), as spelled where it is kept.
                let session = members.iter().find(|m| m.name.eq_ignore_ascii_case(&name));
                Some(match session {
                    Some(member) => member.name.clone(),
                    None => self
                        .roles
                        .lock()
                        .await
                        .role(&name)
                        .map(|r| r.name.clone())
                        .context("Default recipient not found")?,
                })
            }
        };
        let mut channels = self.channels.lock().await;
        match name {
            Some(name) => {
                channels.entry(project).or_default().default_recipient = Some(name);
            }
            None => {
                if let Some(settings) = channels.get_mut(&project) {
                    settings.default_recipient = None;
                }
                channels.retain(|_, s| s != &ChannelSettings::default());
            }
        }
        store::write_json(&self.channels_file, &*channels)?;
        Ok(Reply::Channels {
            channels: channels.clone(),
        })
    }
    /// The LAN view, where a paired bridge reached through a connected SSH
    /// tunnel counts as online (mDNS never sees it).
    async fn lan_view(&self) -> Result<crate::lan::model::LanView> {
        let mut view = self.lan.view().await?;
        let Reply::Remotes { remotes } = self.remotes().await? else {
            return Ok(view);
        };
        for host in view.hosts.iter_mut().filter(|h| h.paired && !h.online) {
            host.online = host.addresses.iter().any(|address| {
                address.ip().is_loopback()
                    && remotes
                        .iter()
                        .any(|r| r.local_port == address.port() && r.state == "connected")
            });
        }
        Ok(view)
    }
    /// Runs one SSH tunnel per remote host while networking is on.
    async fn sync_tunnels(&self) -> Result<()> {
        let Some(own_port) = self.lan.view().await?.network.port else {
            self.tunnels.stop_all().await;
            return Ok(());
        };
        for remote in self.remotes.lock().await.iter() {
            self.tunnels.ensure(remote.clone(), own_port).await;
        }
        Ok(())
    }
    /// Remote hosts and the state of their tunnels.
    async fn remotes(&self) -> Result<Reply> {
        let statuses = self.tunnels.statuses().await;
        let mut views = Vec::new();
        for remote in self.remotes.lock().await.iter() {
            let reported = statuses.get(&remote.host).cloned();
            let answers = tokio::time::timeout(
                Duration::from_millis(500),
                tokio::net::TcpStream::connect(("127.0.0.1", remote.local_port)),
            )
            .await
            .is_ok_and(|c| c.is_ok());
            let state = match reported.as_deref() {
                None => "networking off".to_owned(),
                Some("running") if answers => "connected".to_owned(),
                Some("running") | Some("starting") => "connecting".to_owned(),
                Some(reason) => format!("down: {reason}"),
            };
            views.push(RemoteView {
                host: remote.host.clone(),
                remote_port: remote.remote_port,
                local_port: remote.local_port,
                back_port: remote.back_port,
                state,
            });
        }
        Ok(Reply::Remotes { remotes: views })
    }
    /// Channel members with their roles (REQ-026).
    async fn members(&self, sessions: &[Session]) -> Vec<Member> {
        let book = self.roles.lock().await;
        sessions
            .iter()
            .map(|s| Member {
                session_id: s.session_id.clone(),
                name: s.name.clone(),
                role: book.role_of(&s.name).map(str::to_owned),
            })
            .collect()
    }
    /// Rewrites this bridge's crontab lines (from `before` to `next`), then
    /// saves the schedules; a crontab failure changes nothing.
    async fn store_schedules(&self, before: &[Schedule], next: &[Schedule]) -> Result<()> {
        anyhow::ensure!(
            cfg!(unix),
            "Scheduling uses crontab, which Windows does not have"
        );
        let owned: BTreeSet<Uuid> = before.iter().chain(next).map(|s| s.id).collect();
        let table = schedule::crontab::read(&self.crontab).await?;
        schedule::crontab::write(
            &self.crontab,
            &schedule::render(&table, next, &self.cron_command, &owned),
        )
        .await?;
        store::write_json(&self.schedules_file, &next)
    }
    /// The schedules, whether cron has each, and the user's other crontab lines.
    async fn schedules(&self) -> Result<Reply> {
        let schedules = self.schedules.lock().await.clone();
        let runs = self.last_runs().await;
        if !cfg!(unix) {
            return Ok(Reply::Schedules {
                schedules: schedules
                    .into_iter()
                    .map(|schedule| ScheduleView {
                        last_run: runs.get(&schedule.id).cloned(),
                        schedule,
                        installed: false,
                    })
                    .collect(),
                other_lines: Vec::new(),
                available: false,
                problem: None,
            });
        }
        let table = match schedule::crontab::read(&self.crontab).await {
            Ok(table) => table,
            Err(e) => {
                // Still list the schedules, read-only, with the reason.
                return Ok(Reply::Schedules {
                    schedules: schedules
                        .into_iter()
                        .map(|schedule| ScheduleView {
                            last_run: runs.get(&schedule.id).cloned(),
                            schedule,
                            installed: false,
                        })
                        .collect(),
                    other_lines: Vec::new(),
                    available: true,
                    problem: Some(format!("{e:#}")),
                });
            }
        };
        let owned: BTreeSet<Uuid> = schedules.iter().map(|s| s.id).collect();
        let foreign = schedule::foreign_lines(&table, &owned);
        let lines = schedule::parse(&table);
        let installed: BTreeSet<Uuid> = lines
            .iter()
            .filter_map(|line| match line {
                schedule::Line::Managed(id) => Some(*id),
                schedule::Line::Other(_) => None,
            })
            .collect();
        Ok(Reply::Schedules {
            schedules: schedules
                .into_iter()
                .map(|schedule| ScheduleView {
                    installed: installed.contains(&schedule.id),
                    last_run: runs.get(&schedule.id).cloned(),
                    schedule,
                })
                .collect(),
            other_lines: lines
                .into_iter()
                .filter_map(|line| match line {
                    schedule::Line::Other(text) if text != schedule::HEADER => Some(text),
                    _ => None,
                })
                .chain(foreign)
                .collect(),
            available: true,
            problem: None,
        })
    }
    async fn post(
        &self,
        project: String,
        message: String,
        target: Target,
        scheduled: bool,
    ) -> Result<Reply> {
        let reached = self.recipients(&project, &message, &target).await?;
        let (receipts, failed) = self
            .send_post(&project, &message, &reached, scheduled)
            .await?;
        Ok(Reply::Posted {
            project,
            receipts,
            failed,
            skipped: Vec::new(),
        })
    }
    /// The live members of #`project` a post reaches.
    async fn recipients(
        &self,
        project: &str,
        message: &str,
        target: &Target,
    ) -> Result<Vec<Member>> {
        domain::validate_message(message)?;
        domain::validate_project(project)?;
        anyhow::ensure!(
            !project.contains('/'),
            "A channel is a project name, not a path"
        );
        let Reply::Sessions { sessions, .. } = self.list(None, Some(project.to_owned())).await?
        else {
            anyhow::bail!("Could not list the sessions of #{project}");
        };
        let members = self.members(&sessions).await;
        let default = self
            .channels
            .lock()
            .await
            .get(project)
            .and_then(|c| c.default_recipient.clone());
        let reached = match target {
            Target::Mentions => mentions::resolve(message, &members, project, default.as_deref())?,
            Target::Channel => mentions::resolve("@channel", &members, project, None)?,
            Target::Session(name) => {
                mentions::resolve(&format!("@{name}"), &members, project, None)?
            }
        };
        Ok(reached
            .into_iter()
            .filter_map(|id| members.iter().find(|m| m.session_id == id).cloned())
            .collect())
    }
    /// Sends a post to each member as the user in #`project`.
    async fn send_post(
        &self,
        project: &str,
        message: &str,
        members: &[Member],
        scheduled: bool,
    ) -> Result<(Vec<crate::protocol::Receipt>, Vec<String>)> {
        let (mut receipts, mut failed) = (Vec::new(), Vec::new());
        for member in members {
            let request = Request::UserSend {
                session_id: member.session_id.clone(),
                message: message.to_owned(),
                channel: Some(project.to_owned()),
                scheduled,
                team_report: false,
            };
            match self.ask(|reply| Event::Rpc(request, reply)).await? {
                Reply::Receipt { receipt } => receipts.push(receipt),
                Reply::Error { message } => failed.push(format!("@{}: {message}", member.name)),
                other => failed.push(format!("@{}: unexpected reply {other:?}", member.name)),
            }
        }
        Ok((receipts, failed))
    }
    async fn ask<T>(&self, event: impl FnOnce(oneshot::Sender<T>) -> Event) -> Result<T> {
        let (reply, result) = oneshot::channel();
        self.events.send(event(reply)).await?;
        result.await.context("Bridge stopped before answering")
    }
}
struct Bridge {
    config: Config,
    store: Store,
    messages: BTreeMap<Uuid, Envelope>,
    live: Vec<Session>,
    known: BTreeMap<String, Session>,
    bound: BTreeSet<PathBuf>,
    events: mpsc::Sender<Event>,
    listeners: JoinSet<Result<()>>,
    native_peers: crate::native_peers::NativePeers,
    lan: Lan,
    live_sessions: watch::Sender<Vec<Session>>,
    /// Forwards and status reports in flight; each logs its own failure.
    tasks: JoinSet<Result<()>>,
    /// The user's address on this laptop (REQ-019).
    user: SessionId,
    /// Names of started sessions (REQ-028), shared with `Control`.
    names: Arc<std::sync::Mutex<SessionNames>>,
}
pub async fn run(config: Config) -> Result<()> {
    let store = Store::open(config.state.clone())?;
    let lock = platform::private_options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(config.state.join("daemon.lock"))?;
    lock.try_lock_exclusive()
        .context("Another session-bridge daemon is running")?;
    let socket = config.socket();
    platform::remove_stale_endpoint(&socket)?;
    let mut listener = platform::Listener::bind(&socket)?;
    let (events, mut receiver) = mpsc::channel(128);
    let (inbound, mut inbound_receiver) = mpsc::channel(64);
    let (live_sessions, live) = watch::channel(Vec::new());
    let usage = Sampler::new(config.home.clone())?;
    let lan = Lan::open(
        &config.state,
        config.notify,
        inbound,
        live.clone(),
        usage.clone(),
    )?;
    if let Err(e) = lan.start_saved().await {
        eprintln!("Networking did not start; local messaging continues: {e:#}");
    }
    let names = Arc::new(std::sync::Mutex::new(
        if config.state.join("names.json").exists() {
            store::read_json(&config.state.join("names.json"))?
        } else {
            SessionNames::default()
        },
    ));
    let control = Control {
        events: events.clone(),
        lan: lan.clone(),
        admin: Arc::new(AsyncMutex::new(None)),
        channels: Arc::new(AsyncMutex::new(
            if config.state.join("channels.json").exists() {
                store::read_json(&config.state.join("channels.json"))?
            } else {
                BTreeMap::new()
            },
        )),
        channels_file: config.state.join("channels.json"),
        schedules: Arc::new(AsyncMutex::new(
            if config.state.join("schedules.json").exists() {
                let saved: Vec<serde_json::Value> =
                    store::read_json(&config.state.join("schedules.json"))?;
                saved
                    .into_iter()
                    .filter_map(|raw| {
                        match serde_json::from_value::<Schedule>(schedule::upgrade(raw)) {
                            Ok(s) => Some(s),
                            Err(e) => {
                                eprintln!("Ignoring unreadable schedule: {e}");
                                None
                            }
                        }
                    })
                    .filter(|s| match schedule::validate(s) {
                        Ok(()) => true,
                        Err(e) => {
                            eprintln!("Ignoring invalid schedule {}: {e}", s.id);
                            false
                        }
                    })
                    .collect()
            } else {
                Vec::new()
            },
        )),
        schedules_file: config.state.join("schedules.json"),
        schedule_runs: Arc::new(AsyncMutex::new(
            if config.state.join("schedule_runs.json").exists() {
                store::read_json(&config.state.join("schedule_runs.json"))?
            } else {
                BTreeMap::new()
            },
        )),
        schedule_runs_file: config.state.join("schedule_runs.json"),
        roles: Arc::new(AsyncMutex::new(
            if config.state.join("roles.json").exists() {
                store::read_json::<RoleBook>(&config.state.join("roles.json"))?.with_root()
            } else {
                RoleBook::default().with_root()
            },
        )),
        roles_file: config.state.join("roles.json"),
        crontab: config.crontab.clone(),
        cron_command: schedule::command(
            &std::env::current_exe()?,
            (!config.supervised).then_some(config.state.as_path()),
        ),
        remotes: Arc::new(AsyncMutex::new(
            if config.state.join("remotes.json").exists() {
                store::read_json(&config.state.join("remotes.json"))?
            } else {
                Vec::new()
            },
        )),
        remotes_file: config.state.join("remotes.json"),
        tunnels: Tunnels::new(config.ssh.clone()),
        spawn_settings: Arc::new(AsyncMutex::new(
            if config.state.join("spawn.json").exists() {
                store::read_json(&config.state.join("spawn.json"))?
            } else {
                SpawnSettings::default()
            },
        )),
        spawn_file: config.state.join("spawn.json"),
        pending: Arc::default(),
        names: names.clone(),
        spawners: {
            let stand_in = config.terminal.clone();
            Arc::new(move |terminal| service::for_this_system(stand_in.as_deref(), terminal))
        },
        launch_state: (!config.supervised).then(|| config.state.clone()),
        codex_home: config.sources.codex_home.clone(),
        usage,
        live,
    };
    tokio::spawn(control.clone().wake_codex_queues());
    if let Err(e) = control.sync_tunnels().await {
        eprintln!("SSH tunnels did not start: {e:#}");
    }
    let mut messages = BTreeMap::new();
    for mut envelope in store.messages()? {
        let recovered = domain::recover_delivery(&envelope.delivery);
        if recovered != envelope.delivery {
            envelope.delivery = recovered;
            store.save(&envelope)?;
        }
        messages.insert(envelope.message_id, envelope);
    }
    let known_file = config.state.join("sessions.json");
    let mut known: BTreeMap<String, Session> = if known_file.exists() {
        store::read_json(&known_file)?
    } else {
        BTreeMap::new()
    };
    let user = store.user()?;
    known.insert(user.to_string(), Session::user(&user));
    let mut bridge = Bridge {
        config,
        store,
        messages,
        live: Vec::new(),
        known,
        bound: BTreeSet::new(),
        events: events.clone(),
        listeners: JoinSet::new(),
        native_peers: crate::native_peers::NativePeers::default(),
        lan,
        live_sessions,
        tasks: JoinSet::new(),
        user,
        names,
    };
    bridge.refresh().await?;
    for route in bridge.store.routes()? {
        if let Err(e) = bridge.bind_route(route).await {
            eprintln!("Reply route awaits recovery: {e:#}");
        }
    }
    let mut interval = tokio::time::interval(Duration::from_secs(3));
    let mut handlers = JoinSet::new();
    let mut terminate = platform::Terminate::new()?;
    let mut stopping = false;
    // The menu-bar icon lives as long as this bridge; dropping it ends it.
    #[cfg(target_os = "macos")]
    let _tray = if bridge.config.tray {
        std::env::current_exe()
            .and_then(|exe| {
                tokio::process::Command::new(exe)
                    .arg("tray")
                    .stdin(std::process::Stdio::null())
                    .kill_on_drop(true)
                    .spawn()
            })
            .map_err(|e| eprintln!("Menu bar icon did not start: {e}"))
            .ok()
    } else {
        None
    };
    eprintln!(
        "session-bridge ready: {} ({} live sessions)",
        socket.display(),
        bridge.live.len()
    );
    loop {
        tokio::select! {
            _=tokio::signal::ctrl_c() => break,
            _=terminate.recv() => break,
            accepted=listener.accept()=>{
                let mut stream=accepted?;let control=control.clone();
                handlers.spawn(async move {
                    transport::verify_peer(&stream)?;
                    let request=match transport::read_frame(&mut stream).await {
                        Ok(request)=>request,
                        Err(e)=>{transport::write_frame(&mut stream,&Reply::Error{message:e.to_string()}).await?;return Ok::<_,anyhow::Error>(());}
                    };
                    let peer=platform::peer_pid(&stream);
                    transport::write_frame(&mut stream,&control.call_from(peer,request).await).await
                });
            },
            Some(event)=receiver.recv()=> match event {
                Event::Rpc(request,response)=>{
                    let reply=bridge.handle_request(request).await.unwrap_or_else(|e|Reply::Error{message:format!("{e:#}")});
                    if response.send(reply).is_err(){eprintln!("Client disconnected before bridge response");}
                },
                Event::Peer(route,frame)=>if let Err(e)=bridge.handle_peer(route,*frame).await {eprintln!("Rejected peer message: {e:#}");},
                Event::List{sender,project,reply}=>{
                    if reply.send(bridge.list_local(sender,project).await).is_err(){eprintln!("Client disconnected before listing");}
                },
                Event::QueuedUserPosts(reply)=>{
                    let posts=bridge.messages.values()
                        .filter(|e| e.delivery==Delivery::Queued && e.sender.provider==Provider::User && e.recipient.provider==Provider::Codex)
                        .map(|e| (e.recipient.id,e.message_id)).collect();
                    if reply.send(posts).is_err(){eprintln!("Codex queue check stopped before its answer");}
                },
                Event::RemoteSeen(sessions)=>if let Err(e)=bridge.remember_remote(sessions){eprintln!("Could not record remote sessions: {e:#}");},
                Event::Forwarded{id,outcome}=>if let Err(e)=bridge.complete_forward(id,outcome){eprintln!("Forwarded message {id}: {e:#}");},
                Event::Shutdown=>{eprintln!("Shutdown requested");stopping=true;break;},
            },
            Some(inbound)=inbound_receiver.recv()=>if let Err(e)=bridge.handle_inbound(inbound).await{eprintln!("Paired bridge request failed: {e:#}");},
            Some(result)=bridge.tasks.join_next()=>if let Err(e)=result.context("Background task panicked").and_then(|r|r){eprintln!("{e:#}");},
            _=interval.tick()=>{
                if let Err(e)=bridge.refresh().await {eprintln!("Session discovery failed; retained pending work: {e:#}");}
                for route in bridge.store.routes()? {
                    if !bridge.bound.contains(&route.socket)
                        && let Err(e)=bridge.bind_route(route).await {
                        eprintln!("Reply route recovery pending: {e:#}");
                    }
                }
                let pending:Vec<_>=bridge.messages.values().filter(|m|matches!(m.delivery,Delivery::Pending{..})).map(|m|m.message_id).collect();
                for id in pending {if let Err(e)=bridge.dispatch(id).await{eprintln!("Pending message {id}: {e:#}");}}
            },
            Some(result)=handlers.join_next()=>if let Err(e)=result.context("Client handler panicked").and_then(|r|r){eprintln!("Client request failed: {e:#}");},
            Some(result)=bridge.listeners.join_next()=>{result.context("Reply listener panicked")??;},
        }
    }
    if stopping {
        // Let the request that asked for the shutdown receive its answer.
        let drained = tokio::time::timeout(Duration::from_secs(2), async {
            while handlers.join_next().await.is_some() {}
        });
        if drained.await.is_err() {
            eprintln!("Stopping with client requests still open");
        }
    }
    handlers.abort_all();
    control.tunnels.stop_all().await;
    bridge.lan.stop().await;
    bridge.tasks.abort_all();
    bridge.native_peers.shutdown().await?;
    bridge.listeners.abort_all();
    platform::remove_endpoint(&socket)?;
    for path in bridge.bound {
        platform::remove_endpoint(&path)?;
    }
    drop(lock);
    Ok(())
}
fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
impl Bridge {
    async fn refresh(&mut self) -> Result<()> {
        let mut live = discovery::discover(&self.config.sources).await?;
        self.name_sessions(&mut live).await?;
        for session in &live {
            self.known
                .insert(session.session_id.clone(), session.clone());
        }
        store::write_json(&self.config.state.join("sessions.json"), &self.known)?;
        self.live = live.into_iter().filter(|s| !s.is_subagent()).collect();
        self.live_sessions.send_replace(self.live.clone());
        self.native_peers.sync(&self.config, &self.live).await?;
        Ok(())
    }
    /// Gives started sessions their names (REQ-028).
    async fn name_sessions(&mut self, live: &mut [Session]) -> Result<()> {
        let waiting = self
            .names
            .lock()
            .map_err(|_| anyhow::anyhow!("Session names are unavailable"))?
            .pids
            .clone();
        let parents = if waiting.is_empty() {
            BTreeMap::new()
        } else {
            crate::spawn::process::parent_pids().await?
        };
        let mut names = self
            .names
            .lock()
            .map_err(|_| anyhow::anyhow!("Session names are unavailable"))?;
        // A launched process that ended before it became a session is dropped.
        names.pids.retain(|pid, _| discovery::is_alive(*pid));
        if names.apply(live, &parents, now()?) {
            store::write_json(&self.config.state.join("names.json"), &*names)?;
        }
        Ok(())
    }
    fn project_scope(
        &self,
        sender: Option<&str>,
        project: Option<String>,
    ) -> Result<Option<String>> {
        let project = match (project, sender) {
            (Some(project), _) => Some(project),
            (None, Some(sender)) => Some(
                self.known
                    .get(sender)
                    .context("Cannot resolve caller project: sender session is not registered")?
                    .cwd
                    .to_str()
                    .context("Caller directory is not valid UTF-8")?
                    .to_owned(),
            ),
            (None, None) => None,
        };
        if let Some(project) = &project {
            domain::validate_project(project)?;
        }
        Ok(project)
    }
    /// This laptop's sessions in the resolved scope, and that scope.
    async fn list_local(
        &mut self,
        sender: Option<String>,
        project: Option<String>,
    ) -> Result<(Reply, Option<String>)> {
        self.refresh().await?;
        let project = self.project_scope(sender.as_deref(), project)?;
        let mut recent_messages: Vec<crate::protocol::Receipt> = self
            .messages
            .values()
            .filter(|m| sender.as_ref().is_some_and(|s| s == &m.sender.to_string()))
            .filter(|m| {
                project.as_ref().is_none_or(|p| {
                    self.known
                        .get(&m.recipient.to_string())
                        .is_some_and(|s| s.matches_project(p))
                })
            })
            .map(Into::into)
            .collect();
        recent_messages.sort_by_key(|m| std::cmp::Reverse(m.created_at));
        recent_messages.truncate(10);
        let reply = Reply::Sessions {
            sessions: self
                .live
                .iter()
                .filter(|s| project.as_ref().is_none_or(|p| s.matches_project(p)))
                .cloned()
                .collect(),
            recent_messages,
            unavailable_hosts: Vec::new(),
        };
        Ok((reply, project))
    }
    /// Records sessions listed by paired bridges; a local session always wins.
    fn remember_remote(&mut self, sessions: Vec<Session>) -> Result<()> {
        let mut changed = false;
        for session in sessions.into_iter().filter(|s| s.host.is_some()) {
            if self
                .known
                .get(&session.session_id)
                .is_some_and(|known| known.host.is_none())
            {
                continue;
            }
            self.known.insert(session.session_id.clone(), session);
            changed = true;
        }
        if changed {
            store::write_json(&self.config.state.join("sessions.json"), &self.known)?;
        }
        Ok(())
    }
    async fn handle_inbound(&mut self, inbound: Inbound) -> Result<()> {
        match inbound {
            Inbound::Forward {
                from,
                message,
                reply,
            } => {
                let (answer, accepted) = match self.accept_forward(&from, message) {
                    Ok((delivery, accepted)) => (PeerReply::Accepted { delivery }, accepted),
                    Err(e) => (
                        PeerReply::Error {
                            message: format!("{e:#}"),
                        },
                        None,
                    ),
                };
                if reply.send(answer).is_err() {
                    eprintln!("Paired bridge {} left before the answer", from.label);
                }
                match accepted {
                    Some(id) => self.dispatch(id).await,
                    None => Ok(()),
                }
            }
            Inbound::Status {
                from,
                message_id,
                delivery,
            } => {
                let message = self
                    .messages
                    .get(&message_id)
                    .context("Status for an unknown message")?;
                anyhow::ensure!(
                    self.known
                        .get(&message.recipient.to_string())
                        .and_then(|s| s.host.as_deref())
                        == Some(from.label.as_str()),
                    "Status from {} for a message it does not own",
                    from.label
                );
                match apply_remote_status(&message.delivery, &delivery) {
                    Some(next) => self.set_delivery(message_id, next),
                    None => Ok(()),
                }
            }
        }
    }
    /// Saves a message forwarded by a paired bridge for a local session.
    /// Returns its delivery and, when new, its id for dispatch.
    fn accept_forward(
        &mut self,
        from: &PairedPeer,
        message: ForwardedMessage,
    ) -> Result<(Delivery, Option<Uuid>)> {
        domain::validate_message(&message.message)?;
        if let Some(project) = &message.project {
            domain::validate_project(project)?;
        }
        let sender = &message.sender;
        anyhow::ensure!(
            sender.session_id.provider == sender.agent,
            "Sender agent does not match its session"
        );
        for text in [&sender.project, &sender.name] {
            anyhow::ensure!(
                !text.trim().is_empty() && !text.contains('\0'),
                "Invalid sender metadata"
            );
        }
        anyhow::ensure!(
            sender.cwd.to_str().is_some_and(|c| !c.contains('\0')),
            "Invalid sender directory"
        );
        let recipient = self
            .known
            .get(&message.recipient.to_string())
            .context("Recipient session is unknown on this laptop")?;
        anyhow::ensure!(
            recipient.host.is_none(),
            "Recipient is not a session on this laptop"
        );
        anyhow::ensure!(
            !recipient.is_subagent(),
            "Subagents are managed by their parent sessions"
        );
        if let Some(project) = &message.project {
            anyhow::ensure!(
                recipient.matches_project(project),
                "Recipient does not match project filter: {project}"
            );
        }
        let sender_key = sender.session_id.to_string();
        anyhow::ensure!(
            self.known
                .get(&sender_key)
                .is_none_or(|s| s.host.as_deref() == Some(from.label.as_str())),
            "Sender session belongs to another host"
        );
        self.known.insert(
            sender_key.clone(),
            Session {
                session_id: sender_key,
                agent: sender.agent.clone(),
                project: sender.project.clone(),
                cwd: sender.cwd.clone(),
                name: sender.name.clone(),
                status: "remote".into(),
                kind: "session".into(),
                host: Some(from.label.to_string()),
                pid: 0,
                socket: None,
            },
        );
        store::write_json(&self.config.state.join("sessions.json"), &self.known)?;
        if let Some(existing) = self.messages.get(&message.message_id) {
            anyhow::ensure!(
                existing.sender == message.sender.session_id
                    && existing.recipient == message.recipient
                    && existing.message == message.message,
                "Duplicate message ID with different content"
            );
            return Ok((existing.delivery.clone(), None));
        }
        let delivery = Delivery::Pending {
            reason: format!("Forwarded from {}; waiting for destination", from.label),
        };
        let envelope = Envelope {
            message_id: message.message_id,
            sender: message.sender.session_id,
            recipient: message.recipient,
            message: message.message,
            project: message.project,
            delivery: delivery.clone(),
            created_at: now()?,
            scheduled: message.scheduled,
        };
        self.store.save(&envelope)?;
        self.messages.insert(envelope.message_id, envelope.clone());
        Ok((delivery, Some(envelope.message_id)))
    }
    /// Hands a message for a session on a paired laptop to that laptop's bridge.
    fn forward(&mut self, id: Uuid, host: String) -> Result<()> {
        let message = self.messages.get(&id).context("Unknown message")?.clone();
        let sender = self
            .known
            .get(&message.sender.to_string())
            .context("Missing sender metadata")?
            .clone();
        if sender.host.is_some() {
            return self.set_delivery(
                id,
                Delivery::Refused {
                    reason: "Messages are not relayed between other laptops".into(),
                },
            );
        }
        if let Some(project) = &message.project
            && !self
                .known
                .get(&message.recipient.to_string())
                .is_some_and(|s| s.matches_project(project))
        {
            return self.set_delivery(
                id,
                Delivery::Refused {
                    reason: format!("Recipient no longer matches project filter: {project}"),
                },
            );
        }
        let forwarded = ForwardedMessage {
            message_id: id,
            sender: RemoteSender {
                session_id: message.sender.clone(),
                agent: sender.agent,
                project: sender.project,
                cwd: sender.cwd,
                name: sender.name,
            },
            recipient: message.recipient,
            message: message.message,
            project: message.project,
            scheduled: message.scheduled,
        };
        self.set_delivery(id, Delivery::Dispatching)?;
        let (lan, events) = (self.lan.clone(), self.events.clone());
        self.tasks.spawn(async move {
            let outcome = lan.forward(&host, forwarded).await;
            events
                .send(Event::Forwarded { id, outcome })
                .await
                .context("Bridge stopped during a forward")
        });
        Ok(())
    }
    fn complete_forward(&mut self, id: Uuid, outcome: ForwardOutcome) -> Result<()> {
        let current = &self.messages.get(&id).context("Unknown message")?.delivery;
        match complete_forward(current, &outcome) {
            Some(next) => self.set_delivery(id, next),
            None => Ok(()),
        }
    }
    async fn handle_request(&mut self, request: Request) -> Result<Reply> {
        match request {
            Request::NativePeer { recipient, frame } => {
                let recipient = SessionId::parse(&recipient)?;
                anyhow::ensure!(
                    recipient.provider == Provider::Codex,
                    "Native bridge peers target Codex only"
                );
                anyhow::ensure!(
                    self.known
                        .get(&recipient.to_string())
                        .is_some_and(|s| !s.is_subagent()),
                    "Unknown parent Codex recipient"
                );
                self.handle_peer(
                    Route {
                        socket: self.config.socket(),
                        recipient,
                    },
                    *frame,
                )
                .await?;
                Ok(Reply::Health {
                    pid: std::process::id(),
                })
            }
            Request::Health => Ok(Reply::Health {
                pid: std::process::id(),
            }),
            Request::List { sender, project } => Ok(self.list_local(sender, project).await?.0),
            Request::Network { .. }
            | Request::Lan
            | Request::Pair { .. }
            | Request::PairCode { .. }
            | Request::Unpair { .. }
            | Request::Admin => anyhow::bail!("Network setup is handled outside the message loop"),
            Request::Status => {
                let mut messages: Vec<crate::protocol::Receipt> =
                    self.messages.values().map(Into::into).collect();
                messages.sort_by_key(|m| std::cmp::Reverse(m.created_at));
                messages.truncate(100);
                Ok(Reply::Status {
                    pid: std::process::id(),
                    sessions: self.live.len(),
                    messages,
                })
            }
            Request::Send {
                sender,
                session_id,
                message,
                project,
            } => {
                domain::validate_message(&message)?;
                if let Some(project) = &project {
                    domain::validate_project(project)?;
                }
                let sender = SessionId::parse(&sender)?;
                anyhow::ensure!(
                    sender.provider != Provider::User,
                    "Only the admin page sends messages as the user"
                );
                let recipient = SessionId::parse(&session_id)?;
                self.refresh().await?;
                anyhow::ensure!(
                    self.known.contains_key(&sender.to_string()),
                    "Sender session is not registered"
                );
                let project = self.project_scope(Some(&sender.to_string()), project)?;
                self.enqueue(sender, recipient, message, project, false)
                    .await
            }
            Request::UserSend {
                session_id,
                message,
                channel,
                scheduled,
                team_report: _,
            } => {
                domain::validate_message(&message)?;
                let recipient = SessionId::parse(&session_id)?;
                anyhow::ensure!(
                    recipient.provider != Provider::User,
                    "Send to an agent session"
                );
                self.refresh().await?;
                let (sender, project) = match channel {
                    Some(project) => (self.channel_address(&project)?, Some(project)),
                    None => (self.user.clone(), None),
                };
                self.enqueue(sender, recipient, message, project, scheduled)
                    .await
            }
            Request::ChannelSend { .. }
            | Request::Shutdown
            | Request::Channels
            | Request::ChannelDefault { .. }
            | Request::Schedules
            | Request::SaveSchedule { .. }
            | Request::DeleteSchedule { .. }
            | Request::RunSchedule { .. }
            | Request::Roles
            | Request::SaveRole { .. }
            | Request::DeleteRole { .. }
            | Request::AssignRole { .. }
            | Request::Remotes
            | Request::AddRemote { .. }
            | Request::RemoveRemote { .. }
            | Request::Spawn { .. }
            | Request::Close { .. }
            | Request::ClaimSpawn { .. }
            | Request::SpawnSettings
            | Request::Usage
            | Request::SaveSpawnSettings { .. }
            | Request::Projects
            | Request::ProjectMaster { .. } => {
                anyhow::bail!("Handled outside the message loop")
            }
            // The bridge's part of what waits (REQ-032): every message not
            // handed over yet; Control adds Codex's queue and live sessions.
            Request::Waiting => Ok(Reply::Waiting {
                waiting: crate::waiting::unsent(self.messages.values())
                    .into_iter()
                    .map(|(id, items)| (id, crate::waiting::view(items)))
                    .collect(),
            }),
            Request::Messages => {
                let mut messages: Vec<&Envelope> = self.messages.values().collect();
                messages.sort_by_key(|m| std::cmp::Reverse(m.created_at));
                Ok(Reply::Messages {
                    user: self.user.clone(),
                    messages: messages
                        .into_iter()
                        .take(200)
                        .map(|m| MessageView {
                            message_id: m.message_id,
                            sender: m.sender.clone(),
                            sender_label: self.label(&m.sender),
                            recipient: m.recipient.clone(),
                            recipient_label: self.label(&m.recipient),
                            message: m.message.clone(),
                            delivery: m.delivery.clone(),
                            created_at: m.created_at,
                            scheduled: m.scheduled,
                            channel: [&m.sender, &m.recipient]
                                .iter()
                                .find_map(|id| {
                                    self.known.get(&id.to_string()).and_then(Session::channel)
                                })
                                .map(str::to_owned),
                        })
                        .collect(),
                })
            }
        }
    }
    /// This laptop's user address in a project channel, recorded on first use.
    fn channel_address(&mut self, project: &str) -> Result<SessionId> {
        domain::validate_project(project)?;
        anyhow::ensure!(
            !project.contains('/'),
            "A channel is a project name, not a path"
        );
        let session = Session::in_channel(&self.user, project);
        let id = SessionId::parse(&session.session_id)?;
        if !self.known.contains_key(&session.session_id) {
            self.known.insert(session.session_id.clone(), session);
            store::write_json(&self.config.state.join("sessions.json"), &self.known)?;
        }
        Ok(id)
    }
    /// How the admin page names a party: "you" for this laptop's user, in
    /// direct messages and in channels.
    fn label(&self, id: &SessionId) -> String {
        if id == &self.user || self.is_local_user(id) {
            return "you".into();
        }
        self.known
            .get(&id.to_string())
            .map(Session::label)
            .unwrap_or_else(|| id.to_string())
    }
    /// Saves a new message from a local sender and dispatches it.
    async fn enqueue(
        &mut self,
        sender: SessionId,
        recipient: SessionId,
        message: String,
        project: Option<String>,
        scheduled: bool,
    ) -> Result<Reply> {
        anyhow::ensure!(sender != recipient, "Send to another session, not yourself");
        anyhow::ensure!(
            !self.involves_subagent(&sender, &recipient),
            "Subagents are managed by their parent sessions and cannot use cross-session messaging"
        );
        anyhow::ensure!(
            self.known
                .get(&sender.to_string())
                .is_some_and(|s| s.host.is_none()),
            "Sender must be a session on this laptop"
        );
        anyhow::ensure!(
            self.known.contains_key(&recipient.to_string()),
            "Recipient session is unknown; use list_sessions first"
        );
        if let Some(project) = &project {
            anyhow::ensure!(
                self.known
                    .get(&recipient.to_string())
                    .is_some_and(|s| s.matches_project(project)),
                "Recipient does not match project filter: {project}"
            );
        }
        let id = Uuid::new_v4();
        let envelope = Envelope {
            message_id: id,
            sender,
            recipient,
            message,
            project,
            delivery: Delivery::Pending {
                reason: "Waiting for destination".into(),
            },
            created_at: now()?,
            scheduled,
        };
        self.store.save(&envelope)?;
        self.messages.insert(id, envelope);
        self.dispatch(id).await?;
        Ok(Reply::Receipt {
            receipt: self.messages.get(&id).context("Missing receipt")?.into(),
        })
    }
    async fn dispatch(&mut self, id: Uuid) -> Result<()> {
        let message = self.messages.get(&id).context("Unknown message")?.clone();
        if self.involves_subagent(&message.sender, &message.recipient) {
            return self.set_delivery(
                id,
                Delivery::Refused {
                    reason: "Subagents are managed by their parent sessions".into(),
                },
            );
        }
        if let Some(host) = self
            .known
            .get(&message.recipient.to_string())
            .and_then(|s| s.host.clone())
        {
            return self.forward(id, host);
        }
        if message.recipient.provider == Provider::User {
            // The user's inbox is the admin page, which reads the store.
            return self.set_delivery(
                id,
                if self.is_local_user(&message.recipient) {
                    Delivery::Delivered
                } else {
                    Delivery::Refused {
                        reason: "Unknown user address".into(),
                    }
                },
            );
        }
        let Some(target) = self
            .live
            .iter()
            .find(|s| s.session_id == message.recipient.to_string())
            .cloned()
        else {
            return Ok(());
        };
        if let Some(project) = &message.project
            && (domain::validate_project(project).is_err() || !target.matches_project(project))
        {
            return self.set_delivery(
                id,
                Delivery::Refused {
                    reason: format!("Recipient no longer matches project filter: {project}"),
                },
            );
        }
        let sender = self
            .known
            .get(&message.sender.to_string())
            .context("Missing sender metadata")?
            .clone();
        let text = transport::render_message(&message, &sender)?;
        let route = if let Some(socket) = &target.socket {
            let directory = socket.parent().context("Socket has no parent")?;
            let hash = Uuid::new_v5(&Uuid::NAMESPACE_URL, message.sender.to_string().as_bytes())
                .simple()
                .to_string();
            let native_return = self.native_peers.socket(&sender.session_id);
            let route = Route {
                socket: native_return
                    .clone()
                    .unwrap_or_else(|| directory.join(format!("{}.sock", &hash[..16]))),
                recipient: message.sender.clone(),
            };
            if native_return.is_none() {
                self.store.save_route(&route)?;
                self.bind_route(route.clone()).await?;
            }
            Some(route)
        } else {
            None
        };
        self.set_delivery(id, Delivery::Dispatching)?;
        let delivery = match message.recipient.provider {
            Provider::Codex => {
                crate::codex_sessions::queue(
                    &self.config.codex,
                    &self.config.sources.codex_home,
                    &message.recipient.id.to_string(),
                    &text,
                    message.sender.provider == Provider::User,
                )
                .await
            }
            Provider::Claude => {
                transport::send_claude(
                    target.socket.as_deref().context("Claude has no socket")?,
                    &route.context("Claude has no return route")?.socket,
                    &message,
                    text,
                )
                .await
            }
            Provider::User => Delivery::Refused {
                reason: "The user has no session inbox".into(),
            },
        };
        self.set_delivery(id, delivery)
    }
    fn set_delivery(&mut self, id: Uuid, delivery: Delivery) -> Result<()> {
        let current = self.messages.get(&id).context("Unknown message receipt")?;
        if current.delivery == delivery {
            return Ok(());
        }
        let mut updated = current.clone();
        updated.delivery = delivery;
        self.store.save(&updated)?;
        let reportable = matches!(
            updated.delivery,
            Delivery::Queued
                | Delivery::Delivered
                | Delivery::Held { .. }
                | Delivery::Refused { .. }
                | Delivery::Uncertain { .. }
        );
        if reportable
            && let Some(host) = self
                .known
                .get(&updated.sender.to_string())
                .and_then(|s| s.host.clone())
        {
            let (lan, delivery) = (self.lan.clone(), updated.delivery.clone());
            self.tasks.spawn(async move {
                lan.push_status(&host, id, delivery)
                    .await
                    .with_context(|| format!("Status of {id} not reported to {host}"))
            });
        }
        self.messages.insert(id, updated);
        Ok(())
    }
    /// This laptop's user address or one of its channel addresses.
    fn is_local_user(&self, id: &SessionId) -> bool {
        id.provider == Provider::User
            && self
                .known
                .get(&id.to_string())
                .is_some_and(|s| s.agent == Provider::User && s.host.is_none())
    }
    fn involves_subagent(&self, sender: &SessionId, recipient: &SessionId) -> bool {
        [sender, recipient].iter().any(|id| {
            self.known
                .get(&id.to_string())
                .is_some_and(Session::is_subagent)
        })
    }
    async fn bind_route(&mut self, route: Route) -> Result<()> {
        if self.bound.contains(&route.socket) {
            return Ok(());
        }
        store::ensure_private_dir(route.socket.parent().context("No reply socket directory")?)?;
        platform::remove_stale_endpoint(&route.socket)?;
        let mut listener = platform::Listener::bind(&route.socket)?;
        self.bound.insert(route.socket.clone());
        let events = self.events.clone();
        self.listeners.spawn(async move {
            loop {
                let mut stream = listener.accept().await?;
                let frame = async {
                    transport::verify_peer(&stream)?;
                    transport::read_frame::<PeerFrame>(&mut stream).await
                }
                .await;
                match frame {
                    Ok(frame) => {
                        events
                            .send(Event::Peer(route.clone(), Box::new(frame)))
                            .await?
                    }
                    Err(e) => eprintln!("Reply frame rejected: {e:#}"),
                }
            }
        });
        Ok(())
    }
    async fn handle_peer(&mut self, route: Route, frame: PeerFrame) -> Result<()> {
        self.refresh().await?;
        let from = frame
            .from
            .as_deref()
            .and_then(|v| v.strip_prefix("uds:"))
            .context("Missing peer return address")?;
        let sender = self
            .live
            .iter()
            .find(|s| s.agent == Provider::Claude && s.socket.as_deref() == Some(Path::new(from)))
            .context("Reply is not from a known live Claude session")?
            .clone();
        if frame.kind == "control" {
            anyhow::ensure!(
                frame.action.as_deref() == Some("peer_message_status"),
                "Unsupported peer control action"
            );
            let id = Uuid::parse_str(
                frame
                    .orig_msg_id
                    .as_deref()
                    .context("Uncorrelated peer status")?,
            )?;
            let original = self
                .messages
                .get(&id)
                .context("Unknown status correlation")?;
            anyhow::ensure!(
                original.sender == route.recipient
                    && original.recipient.to_string() == sender.session_id,
                "Status route mismatch"
            );
            let reason = frame
                .reason
                .unwrap_or_else(|| "Claude inbound policy".into());
            let delivery = match frame.status.as_deref() {
                Some("held") => Delivery::Held { reason },
                Some("delivered") => Delivery::Delivered,
                Some("refused" | "denied" | "expired" | "dropped") => Delivery::Refused { reason },
                _ => anyhow::bail!("Unsupported peer delivery status"),
            };
            self.set_delivery(id, delivery)?;
            return Ok(());
        }
        anyhow::ensure!(frame.kind == "user", "Unsupported peer message type");
        let message = frame
            .message
            .context("Peer message has no content")?
            .content;
        domain::validate_message(&message)?;
        let id = match frame.msg_id.or(frame.uuid) {
            Some(id) => Uuid::parse_str(&id)?,
            None => Uuid::new_v4(),
        };
        let sender = SessionId::parse(&sender.session_id)?;
        if let Some(existing) = self.messages.get(&id) {
            anyhow::ensure!(
                existing.sender == sender
                    && existing.recipient == route.recipient
                    && existing.message == message,
                "Duplicate message ID with different content"
            );
            return Ok(());
        }
        let envelope = Envelope {
            message_id: id,
            sender,
            recipient: route.recipient,
            message,
            project: None,
            delivery: Delivery::Pending {
                reason: "Reply awaits destination".into(),
            },
            created_at: now()?,
            scheduled: false,
        };
        self.store.save(&envelope)?;
        self.messages.insert(id, envelope);
        self.dispatch(id).await
    }
}
