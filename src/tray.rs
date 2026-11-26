//! The macOS menu-bar icon of a running bridge (REQ-022). The menu is a pure
//! function of the bridge's state; `app` (macOS only) draws it and runs the
//! chosen actions.
#[cfg(target_os = "macos")]
pub mod app;

/// What the icon knows about its bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bridge {
    Running { sessions: usize, networking: bool },
    NotResponding,
}

/// Who keeps the bridge running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    /// The LaunchAgent for this home: starts at login, restarted by launchd.
    Installed,
    /// Started from a shell; nothing restarts it.
    FromShell,
    /// Started from a shell on its own state directory (SESSION_BRIDGE_STATE):
    /// the installed service is not its business.
    Separate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    OpenAdmin,
    Install,
    Uninstall,
    ShutDown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// A line of status; not clickable.
    Info(String),
    Separator,
    Button {
        label: String,
        action: Action,
        enabled: bool,
    },
}

/// A question asked before an action that stops the bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirmation {
    pub title: String,
    pub message: String,
    pub button: String,
}

pub fn menu(version: &str, bridge: &Bridge, service: Service) -> Vec<Entry> {
    let running = matches!(bridge, Bridge::Running { .. });
    let state = match bridge {
        Bridge::Running { sessions: 1, .. } => "running, 1 session".to_owned(),
        Bridge::Running { sessions, .. } => format!("running, {sessions} sessions"),
        Bridge::NotResponding => "not responding".to_owned(),
    };
    let networking = match bridge {
        Bridge::Running {
            networking: true, ..
        } => "Networking on",
        Bridge::Running { .. } => "Networking off",
        Bridge::NotResponding => "Networking unknown",
    };
    let supervision = match service {
        Service::Installed => "starts at login",
        Service::FromShell => "running from a shell",
        Service::Separate => "separate bridge from a shell",
    };
    let mut entries = vec![
        Entry::Info(format!("session-bridge {version}: {state}")),
        Entry::Info(format!("{networking}; {supervision}")),
        Entry::Separator,
        Entry::Button {
            label: "Open admin".into(),
            action: Action::OpenAdmin,
            enabled: running,
        },
    ];
    let service_item = match service {
        Service::Installed => Some(("Uninstall service...", Action::Uninstall)),
        Service::FromShell => Some(("Install service", Action::Install)),
        Service::Separate => None,
    };
    if let Some((label, action)) = service_item {
        entries.push(Entry::Button {
            label: label.into(),
            action,
            enabled: true,
        });
    }
    entries.extend([
        Entry::Separator,
        Entry::Button {
            label: "Shut down...".into(),
            action: Action::ShutDown,
            enabled: true,
        },
    ]);
    entries
}

pub fn confirmation(action: Action, service: Service) -> Option<Confirmation> {
    let silent = "Agents cannot message each other through the bridge until it runs again.";
    match (action, service) {
        (Action::ShutDown, Service::Installed) => Some(Confirmation {
            title: "Shut down session-bridge?".into(),
            message: format!(
                "{silent} It starts again at your next login, or now with: session-bridge install"
            ),
            button: "Shut down".into(),
        }),
        (Action::ShutDown, Service::FromShell | Service::Separate) => Some(Confirmation {
            title: "Shut down session-bridge?".into(),
            message: format!("{silent} Start it again with: session-bridge daemon"),
            button: "Shut down".into(),
        }),
        (Action::Uninstall, _) => Some(Confirmation {
            title: "Uninstall the background service?".into(),
            message: format!(
                "The bridge stops now and no longer starts at login. {silent} Messages, pairing and MCP entries stay; reinstall with: session-bridge install"
            ),
            button: "Uninstall".into(),
        }),
        (Action::OpenAdmin | Action::Install, _) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn button(label: &str, action: Action, enabled: bool) -> Entry {
        Entry::Button {
            label: label.into(),
            action,
            enabled,
        }
    }

    /* REQ-022: The menu shows the bridge's state and offers open, uninstall
    and shut down for the installed service. */
    #[test]
    fn menu_for_the_installed_service() {
        assert_eq!(
            menu(
                "0.3.0",
                &Bridge::Running {
                    sessions: 6,
                    networking: false
                },
                Service::Installed
            ),
            [
                Entry::Info("session-bridge 0.3.0: running, 6 sessions".into()),
                Entry::Info("Networking off; starts at login".into()),
                Entry::Separator,
                button("Open admin", Action::OpenAdmin, true),
                button("Uninstall service...", Action::Uninstall, true),
                Entry::Separator,
                button("Shut down...", Action::ShutDown, true),
            ]
        );
    }

    /* REQ-022: A bridge run from a shell offers to install the service; an
    unresponsive bridge cannot be opened. */
    #[test]
    fn menu_for_a_shell_bridge() {
        let entries = menu(
            "0.3.0",
            &Bridge::Running {
                sessions: 1,
                networking: true,
            },
            Service::FromShell,
        );
        assert_eq!(
            entries[0],
            Entry::Info("session-bridge 0.3.0: running, 1 session".into())
        );
        assert_eq!(
            entries[1],
            Entry::Info("Networking on; running from a shell".into())
        );
        assert_eq!(entries[4], button("Install service", Action::Install, true));
        let stuck = menu("0.3.0", &Bridge::NotResponding, Service::FromShell);
        assert_eq!(
            stuck[0],
            Entry::Info("session-bridge 0.3.0: not responding".into())
        );
        assert_eq!(stuck[3], button("Open admin", Action::OpenAdmin, false));
    }

    /* REQ-022: A separate bridge (its own state directory) never offers to
    install or uninstall the service, which belongs to the default state. */
    #[test]
    fn menu_for_a_separate_bridge() {
        let entries = menu(
            "0.3.0",
            &Bridge::Running {
                sessions: 0,
                networking: false,
            },
            Service::Separate,
        );
        assert_eq!(
            entries[1],
            Entry::Info("Networking off; separate bridge from a shell".into())
        );
        assert!(!entries.iter().any(|e| matches!(
            e,
            Entry::Button {
                action: Action::Install | Action::Uninstall,
                ..
            }
        )));
        assert!(entries.contains(&button("Shut down...", Action::ShutDown, true)));
        assert!(confirmation(Action::ShutDown, Service::Separate).is_some());
    }

    /* REQ-022: Actions that stop the bridge ask first and say what happens next. */
    #[test]
    fn asks_before_stopping() {
        let installed = confirmation(Action::ShutDown, Service::Installed).unwrap();
        assert_eq!(installed.title, "Shut down session-bridge?");
        assert!(
            installed
                .message
                .contains("starts again at your next login")
        );
        assert_eq!(installed.button, "Shut down");
        let shell = confirmation(Action::ShutDown, Service::FromShell).unwrap();
        assert!(shell.message.contains("session-bridge daemon"));
        let uninstall = confirmation(Action::Uninstall, Service::Installed).unwrap();
        assert!(uninstall.message.contains("no longer starts at login"));
        assert_eq!(uninstall.button, "Uninstall");
        assert_eq!(confirmation(Action::OpenAdmin, Service::Installed), None);
        assert_eq!(confirmation(Action::Install, Service::FromShell), None);
    }
}
