//! The spawn service (REQ-028): how each operating system opens a started
//! session. The bridge depends on [`Spawner`] only; [`for_this_system`]
//! picks the implementation.
//!
//! - macOS: a Terminal or iTerm2 window ([`MacTerminal`]).
//! - Windows: a `cmd` console window ([`WindowsConsole`]).
//! - Linux: off screen in tmux, screen or a detached `script` pseudo-terminal
//!   ([`LinuxDetached`]).
//! - Tests: a stand-in program ([`StandIn`]).
//!
//! Every implementation runs only `<exe> launch <token> [<state>]`: the launched
//! process claims its directory, name and prompt from the bridge.
use super::{Command as Planned, Detached, Os, Terminal, terminal_command};
use anyhow::{Context, Result};
use std::{future::Future, pin::Pin, process::Stdio, time::Duration};
use tokio::{process::Command, time::timeout};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What a spawner starts, titled `title` (the
/// session's name: letters, digits, '.', '_', '-').
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    /// From [`super::launch_argv`].
    pub argv: Vec<String>,
    pub title: String,
}

/// Opens a started session where the user can find it.
pub trait Spawner: Send + Sync {
    /// Where sessions open, for messages: "Terminal", "cmd", "tmux", ...
    fn place(&self) -> &'static str;
    /// Returns once the session's window or detached session exists.
    fn open<'a>(&'a self, launch: &'a Launch) -> BoxFuture<'a, Result<()>>;
}

/// The spawner of this system: `stand_in` (tests) when set, else the
/// system's own, with the user's macOS terminal choice.
pub fn for_this_system(stand_in: Option<&str>, terminal: Terminal) -> Result<Box<dyn Spawner>> {
    if let Some(program) = stand_in {
        return Ok(Box::new(StandIn {
            program: program.into(),
        }));
    }
    match current_os() {
        Os::MacOs => Ok(Box::new(MacTerminal { terminal })),
        Os::Windows => Ok(Box::new(WindowsConsole)),
        Os::Linux(kind) => Ok(Box::new(LinuxDetached { kind })),
        Os::Other => anyhow::bail!(
            "Starting sessions needs Terminal or iTerm2 (macOS), cmd (Windows), or tmux, screen or script (Linux)"
        ),
    }
}

/// This system, and on Linux the first of tmux, screen and script on PATH.
pub fn current_os() -> Os {
    if cfg!(target_os = "macos") {
        Os::MacOs
    } else if cfg!(windows) {
        Os::Windows
    } else if cfg!(target_os = "linux") {
        let on_path = |name: &str| {
            std::env::var_os("PATH").is_some_and(|path| {
                std::env::split_paths(&path).any(|dir| dir.join(name).is_file())
            })
        };
        if on_path("tmux") {
            Os::Linux(Detached::Tmux)
        } else if on_path("screen") {
            Os::Linux(Detached::Screen)
        } else if on_path("script") && on_path("setsid") {
            Os::Linux(Detached::Script)
        } else {
            Os::Other
        }
    } else {
        Os::Other
    }
}

pub struct MacTerminal {
    pub terminal: Terminal,
}
impl Spawner for MacTerminal {
    fn place(&self) -> &'static str {
        match self.terminal {
            Terminal::Terminal => "Terminal",
            Terminal::Iterm => "iTerm2",
        }
    }
    fn open<'a>(&'a self, launch: &'a Launch) -> BoxFuture<'a, Result<()>> {
        Box::pin(run_planned(Os::MacOs, self.terminal, launch))
    }
}

pub struct WindowsConsole;
impl Spawner for WindowsConsole {
    fn place(&self) -> &'static str {
        "cmd"
    }
    fn open<'a>(&'a self, launch: &'a Launch) -> BoxFuture<'a, Result<()>> {
        Box::pin(run_planned(Os::Windows, Terminal::default(), launch))
    }
}

pub struct LinuxDetached {
    pub kind: Detached,
}
impl Spawner for LinuxDetached {
    fn place(&self) -> &'static str {
        match self.kind {
            Detached::Tmux => "tmux",
            Detached::Screen => "screen",
            Detached::Script => "script",
        }
    }
    fn open<'a>(&'a self, launch: &'a Launch) -> BoxFuture<'a, Result<()>> {
        Box::pin(run_planned(
            Os::Linux(self.kind),
            Terminal::default(),
            launch,
        ))
    }
}

/// Runs `program <title> <exe> launch <token>`; tests use it in place of a terminal.
pub struct StandIn {
    pub program: String,
}
impl Spawner for StandIn {
    fn place(&self) -> &'static str {
        "stand-in terminal"
    }
    fn open<'a>(&'a self, launch: &'a Launch) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            run(&Planned {
                program: self.program.clone(),
                args: std::iter::once(launch.title.clone())
                    .chain(launch.argv.iter().cloned())
                    .collect(),
            })
            .await
        })
    }
}

async fn run_planned(os: Os, terminal: Terminal, launch: &Launch) -> Result<()> {
    let planned = terminal_command(os, terminal, &launch.argv, &launch.title)
        .context("No terminal for this system")?;
    run(&planned).await
}

async fn run(planned: &Planned) -> Result<()> {
    let mut command = Command::new(&planned.program);
    // cmd parses its own quotes, so its arguments go as written.
    #[cfg(windows)]
    for arg in &planned.args {
        command.raw_arg(arg);
    }
    #[cfg(not(windows))]
    command.args(&planned.args);
    let output = timeout(
        Duration::from_secs(20),
        command.stdin(Stdio::null()).kill_on_drop(true).output(),
    )
    .await
    .with_context(|| format!("{} did not answer", planned.program))?
    .with_context(|| format!("Could not run {}", planned.program))?;
    anyhow::ensure!(
        output.status.success(),
        "The terminal did not open: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}
