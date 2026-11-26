//! The AppKit side of the menu-bar icon (macOS, REQ-022): a child process of
//! the bridge. It reads the bridge's state every few seconds, shows the menu
//! that `super::menu` describes, and runs the chosen action. It exits when
//! its bridge is gone.
use super::{Action, Bridge, Entry, Service, confirmation, menu};
use crate::{
    daemon::Config,
    install::{self, ServiceState},
    lan::notify::quoted,
    protocol::{Reply, Request},
    transport,
};
use anyhow::{Context, Result};
use std::{
    path::PathBuf,
    process::Command,
    sync::Mutex,
    time::{Duration, Instant},
};
use tao::{
    event::{Event, StartCause},
    event_loop::{ControlFlow, EventLoopBuilder},
    platform::macos::{ActivationPolicy, EventLoopExtMacOS},
};
use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder,
    menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem},
};

const REFRESH: Duration = Duration::from_secs(3);

/// A 36x36 template image: a bridge arch over a deck. Only alpha matters;
/// macOS tints template images for light and dark menu bars.
pub fn icon_pixels() -> (Vec<u8>, u32) {
    const SIZE: u32 = 36;
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
    let (cx, cy, radius) = (18.0f32, 27.0f32, 15.0f32);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let distance = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
            let arch = fy <= cy && (radius - 3.0..=radius).contains(&distance);
            let deck = (18..21).contains(&y) && (2..34).contains(&x);
            let pillars = (21..30).contains(&y) && ((6..9).contains(&x) || (27..30).contains(&x));
            if arch || deck || pillars {
                let at = ((y * SIZE + x) * 4) as usize;
                rgba[at + 3] = 255;
            }
        }
    }
    (rgba, SIZE)
}

enum UserEvent {
    Menu(MenuEvent),
}

struct Tray {
    config: Config,
    home: PathBuf,
    runtime: tokio::runtime::Runtime,
    icon: Option<TrayIcon>,
    shown: Vec<Entry>,
    actions: Vec<(MenuId, Action)>,
    service: Service,
}

impl Tray {
    fn ask(&self, request: Request) -> Result<Reply> {
        let socket = self.config.socket();
        self.runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(2), async {
                let mut stream = transport::connect(&socket).await?;
                transport::write_frame(&mut stream, &request).await?;
                transport::read_frame::<Reply>(&mut stream).await
            })
            .await
            .context("The bridge did not answer in time")?
        })
    }
    fn bridge(&self) -> Bridge {
        let sessions = match self.ask(Request::Status) {
            Ok(Reply::Status { sessions, .. }) => sessions,
            _ => return Bridge::NotResponding,
        };
        let networking = matches!(
            self.ask(Request::Lan),
            Ok(Reply::Lan { lan }) if lan.network.enabled
        );
        Bridge::Running {
            sessions,
            networking,
        }
    }
    fn service(&self) -> Service {
        if !self.config.supervised {
            return Service::Separate;
        }
        match self.runtime.block_on(install::service_state(&self.home)) {
            Ok(ServiceState::Installed) => Service::Installed,
            Ok(_) => Service::FromShell,
            Err(e) => {
                eprintln!("Menu bar: service state unknown: {e:#}");
                self.service
            }
        }
    }
    fn refresh(&mut self) {
        self.service = self.service();
        let entries = menu(env!("CARGO_PKG_VERSION"), &self.bridge(), self.service);
        if entries == self.shown {
            return;
        }
        let lines: Vec<String> = entries
            .iter()
            .map(|entry| match entry {
                Entry::Info(text) => text.clone(),
                Entry::Separator => "-".into(),
                Entry::Button { label, enabled, .. } => {
                    format!("[{label}]{}", if *enabled { "" } else { " (disabled)" })
                }
            })
            .collect();
        eprintln!("Menu bar: {}", lines.join(" | "));
        let built = Menu::new();
        let mut actions = Vec::new();
        for entry in &entries {
            let appended = match entry {
                Entry::Info(text) => built.append(&MenuItem::new(text, false, None)),
                Entry::Separator => built.append(&PredefinedMenuItem::separator()),
                Entry::Button {
                    label,
                    action,
                    enabled,
                } => {
                    let item = MenuItem::new(label, *enabled, None);
                    actions.push((item.id().clone(), *action));
                    built.append(&item)
                }
            };
            if let Err(e) = appended {
                eprintln!("Menu bar: {e}");
            }
        }
        if let Some(icon) = &self.icon {
            icon.set_menu(Some(Box::new(built)));
        }
        self.shown = entries;
        self.actions = actions;
    }
    fn perform(&mut self, id: &MenuId) {
        let Some(action) = self.actions.iter().find(|(i, _)| i == id).map(|(_, a)| *a) else {
            return;
        };
        if let Some(question) = confirmation(action, self.service)
            && !confirmed(&question.title, &question.message, &question.button)
        {
            return;
        }
        let result = match action {
            Action::OpenAdmin => self.open_admin(),
            Action::Install => self.install(),
            Action::Uninstall => run_self(&["uninstall"]),
            Action::ShutDown => self
                .runtime
                .block_on(install::stop(&self.config, &self.home))
                .map(|said| eprintln!("{said}")),
        };
        if let Err(e) = result {
            alert("session-bridge", &format!("{e:#}"));
        }
        self.refresh();
    }
    fn open_admin(&self) -> Result<()> {
        let Reply::Admin { url } = self.ask(Request::Admin)? else {
            anyhow::bail!("The bridge did not return the admin page");
        };
        let status = Command::new("/usr/bin/open").arg(&url).status()?;
        anyhow::ensure!(status.success(), "Could not open the admin page");
        Ok(())
    }
    /// Registers the LaunchAgent; a bridge from a shell on the same state then
    /// stops so the service takes over (launchd retries until the lock frees).
    fn install(&self) -> Result<()> {
        run_self(&["install"])?;
        if self.config.supervised {
            self.runtime
                .block_on(transport_shutdown(&self.config))
                .context("Installed; stop this bridge to let the service take over")?;
        }
        Ok(())
    }
}

async fn transport_shutdown(config: &Config) -> Result<()> {
    let mut stream = transport::connect(&config.socket()).await?;
    transport::write_frame(&mut stream, &Request::Shutdown).await?;
    transport::read_frame::<Reply>(&mut stream).await?;
    Ok(())
}

/// Runs this executable with `args` (install, uninstall), with the same environment.
fn run_self(args: &[&str]) -> Result<()> {
    let output = Command::new(std::env::current_exe()?).args(args).output()?;
    anyhow::ensure!(
        output.status.success(),
        "session-bridge {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

/// Asks with a standard dialog; true when the user chose `button`.
fn confirmed(title: &str, message: &str, button: &str) -> bool {
    let script = format!(
        "display dialog {} with title {} buttons {{\"Cancel\", {}}} default button \"Cancel\" cancel button \"Cancel\" with icon caution",
        quoted(message, '"'),
        quoted(title, '"'),
        quoted(button, '"')
    );
    Command::new("/usr/bin/osascript")
        .args(["-e", &script])
        .output()
        .is_ok_and(|o| o.status.success())
}

fn alert(title: &str, message: &str) {
    let script = format!(
        "display alert {} message {} as warning",
        quoted(title, '"'),
        quoted(message, '"')
    );
    if let Err(e) = Command::new("/usr/bin/osascript")
        .args(["-e", &script])
        .output()
    {
        eprintln!("Menu bar: {message} ({e})");
    }
}

/// Runs the menu-bar icon until the bridge that started it is gone.
pub fn run(config: Config, home: PathBuf) -> Result<()> {
    let parent = std::os::unix::process::parent_id();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    event_loop.set_activation_policy(ActivationPolicy::Accessory);
    let proxy = Mutex::new(event_loop.create_proxy());
    MenuEvent::set_event_handler(Some(std::sync::Arc::new(move |event| {
        if let Ok(proxy) = proxy.lock()
            && proxy.send_event(UserEvent::Menu(event)).is_err()
        {
            eprintln!("Menu bar: event loop closed");
        }
    })));
    let mut tray = Tray {
        config,
        home,
        runtime,
        icon: None,
        shown: Vec::new(),
        actions: Vec::new(),
        service: Service::FromShell,
    };
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + REFRESH);
        match event {
            Event::NewEvents(StartCause::Init) => {
                let (rgba, size) = icon_pixels();
                let icon = Icon::from_rgba(rgba, size, size)
                    .map_err(anyhow::Error::from)
                    .and_then(|icon| {
                        TrayIconBuilder::new()
                            .with_icon_templated(icon)
                            .with_tooltip("session-bridge")
                            .build()
                            .map_err(anyhow::Error::from)
                    });
                match icon {
                    Ok(icon) => tray.icon = Some(icon),
                    Err(e) => {
                        eprintln!("Menu bar icon unavailable: {e:#}");
                        *control_flow = ControlFlow::Exit;
                        return;
                    }
                }
                tray.refresh();
            }
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                if std::os::unix::process::parent_id() != parent {
                    *control_flow = ControlFlow::Exit;
                    return;
                }
                tray.refresh();
            }
            Event::UserEvent(UserEvent::Menu(event)) => tray.perform(&event.id),
            _ => {}
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /* REQ-022: The icon is a square template image with drawn and clear pixels. */
    #[test]
    fn draws_the_icon() {
        let (rgba, size) = icon_pixels();
        assert_eq!(rgba.len(), (size * size * 4) as usize);
        let drawn = rgba.chunks(4).filter(|p| p[3] == 255).count();
        assert!(drawn > 100 && drawn < (size * size) as usize / 2, "{drawn}");
        assert!(rgba.chunks(4).all(|p| p[..3] == [0, 0, 0]));
    }
}
