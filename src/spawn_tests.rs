use super::*;
use proptest::prelude::*;
use std::path::{Path, PathBuf};

const ROOT_IN_MONO: Caller = Caller::Session {
    role: Some("root"),
    project: "mono",
};

/* REQ-028: A session is named {short}-{role}-{n} with the lowest free n; the
short name is the user's, else the project name made safe. */
#[test]
fn names_sessions_by_project_role_and_count() {
    let mut settings = SpawnSettings::default();
    assert_eq!(short_name("mono", &settings), "mono");
    assert_eq!(short_name("my project!", &settings), "my-project");
    assert_eq!(short_name("...", &settings), "project");
    settings
        .short_names
        .insert("session-bridge".into(), "sb".into());
    assert_eq!(short_name("session-bridge", &settings), "sb");
    assert_eq!(next_name("sb", "reviewer", []), "sb-reviewer-1");
    assert_eq!(
        next_name(
            "sb",
            "reviewer",
            ["sb-reviewer-1", "SB-REVIEWER-3", "master"]
        ),
        "sb-reviewer-2"
    );
    assert!(validate_short_name("sb").is_ok());
    for bad in ["", "-sb", "s b", "s'b", &"x".repeat(33)] {
        assert_eq!(
            validate_short_name(bad),
            Err(SpawnError::ShortName),
            "{bad}"
        );
    }
}

/* REQ-028: The user starts and closes any session; a root session starts
non-root sessions in its own project, below the limit, and closes non-root
sessions there; other sessions do neither. */
#[test]
fn only_root_sessions_manage_sessions_and_never_root() {
    assert_eq!(may_spawn(Caller::User, "mono", "root", 50), Ok(()));
    assert_eq!(may_close(Caller::User, "mono", Some("root")), Ok(()));
    assert_eq!(may_spawn(ROOT_IN_MONO, "mono", "reviewer", 2), Ok(()));
    assert_eq!(may_close(ROOT_IN_MONO, "mono", Some("reviewer")), Ok(()));
    assert_eq!(may_close(ROOT_IN_MONO, "mono", None), Ok(()));
    assert_eq!(
        may_spawn(ROOT_IN_MONO, "mono", "ROOT", 2),
        Err(SpawnError::RootRole)
    );
    assert_eq!(
        may_spawn(ROOT_IN_MONO, "other", "reviewer", 2),
        Err(SpawnError::OtherProject)
    );
    assert_eq!(
        may_spawn(ROOT_IN_MONO, "mono", "reviewer", ROOT_LIMIT),
        Err(SpawnError::TooMany("mono".into(), ROOT_LIMIT))
    );
    assert_eq!(
        may_close(ROOT_IN_MONO, "mono", Some("root")),
        Err(SpawnError::CloseRoot)
    );
    assert_eq!(
        may_close(ROOT_IN_MONO, "other", None),
        Err(SpawnError::OtherProject)
    );
    for role in [None, Some("reviewer")] {
        let caller = Caller::Session {
            role,
            project: "mono",
        };
        assert_eq!(
            may_spawn(caller, "mono", "reviewer", 0),
            Err(SpawnError::NotRoot)
        );
        assert_eq!(may_close(caller, "mono", None), Err(SpawnError::NotRoot));
    }
}

/* REQ-028: Agents start with their name and the role's instructions as the
first prompt, and without any permission flag. */
#[test]
fn starts_agents_without_permission_flags() {
    assert_eq!(
        agent_args(Agent::Claude, "mono-qa-1", "Check it"),
        ["-n", "mono-qa-1", "Check it"]
    );
    assert_eq!(
        agent_args(Agent::Codex, "mono-qa-1", "Check it"),
        ["Check it"]
    );
    for agent in [Agent::Claude, Agent::Codex] {
        assert!(
            !agent_args(agent, "n", "p")
                .iter()
                .any(|a| a.contains("dangerously") || a.contains("yolo") || a.contains("bypass"))
        );
    }
}

/* REQ-028: Every system runs only this executable with `launch <token>`
(and a separate bridge's state directory); names and paths are quoted for
the shell, AppleScript and cmd. */
#[test]
fn opens_terminals_with_the_launch_command_only() {
    let token = uuid::Uuid::nil();
    let exe = PathBuf::from("/Users/a b/it's/session-bridge");
    let argv = launch_argv(&exe, token, None);
    assert_eq!(
        argv,
        [exe.to_string_lossy().as_ref(), "launch", &token.to_string()]
    );
    let with_state = launch_argv(&exe, token, Some(Path::new("/tmp/s")));
    assert_eq!(with_state[3], "/tmp/s");
    let line = format!("'/Users/a b/it'\\''s/session-bridge' 'launch' '{token}'");
    let mac = terminal_command(Os::MacOs, Terminal::Terminal, &argv, "mono-qa-1").unwrap();
    assert_eq!(mac.program, "/usr/bin/osascript");
    assert_eq!(
        mac.args[1],
        format!(
            "tell application \"Terminal\"\nactivate\nset t to do script \"exec {}\"\nset custom title of t to \"mono-qa-1\"\nend tell",
            line.replace('\\', "\\\\")
        )
    );
    let iterm = terminal_command(Os::MacOs, Terminal::Iterm, &argv, "t").unwrap();
    assert!(iterm.args[1].starts_with("tell application \"iTerm\""));
    assert!(iterm.args[1].contains(&format!("'launch' '{token}'")));
    let windows = terminal_command(Os::Windows, Terminal::Terminal, &argv, "t").unwrap();
    assert_eq!(windows.program, "cmd.exe");
    assert_eq!(
        windows.args,
        [
            "/c",
            "start",
            "\"t\"",
            "\"/Users/a b/it's/session-bridge\"",
            "\"launch\"",
            &format!("\"{token}\"")
        ]
    );
    let dotted = terminal_command(
        Os::Linux(Detached::Tmux),
        Terminal::Terminal,
        &argv,
        "a.b-r-1",
    )
    .unwrap();
    assert_eq!(dotted.args[3], "a_b-r-1");
    let tmux = terminal_command(Os::Linux(Detached::Tmux), Terminal::Terminal, &argv, "t").unwrap();
    assert_eq!(tmux.args, ["new-session", "-d", "-s", "t", line.as_str()]);
    let screen =
        terminal_command(Os::Linux(Detached::Screen), Terminal::Terminal, &argv, "t").unwrap();
    assert_eq!(screen.args[..2], ["-dmS", "t"]);
    assert_eq!(screen.args[2..], argv[..]);
    let script =
        terminal_command(Os::Linux(Detached::Script), Terminal::Terminal, &argv, "t").unwrap();
    assert_eq!(script.program, "setsid");
    assert_eq!(
        script.args,
        ["-f", "script", "-qfc", line.as_str(), "/dev/null"]
    );
    assert_eq!(
        terminal_command(Os::Other, Terminal::Terminal, &argv, "t"),
        None
    );
}

proptest! {
    /* REQ-028: The chosen name is never taken and follows the pattern. */
    #[test]
    fn next_name_is_free(taken in proptest::collection::vec(1u32..12, 0..12)) {
        let names: Vec<String> = taken.iter().map(|n| format!("p-r-{n}")).collect();
        let name = next_name("p", "r", names.iter().map(String::as_str));
        prop_assert!(!names.contains(&name));
        let n: u32 = name.strip_prefix("p-r-").unwrap().parse().unwrap();
        prop_assert!((1..n).all(|k| taken.contains(&k)));
    }
}

#[cfg(unix)]
proptest! {
    /* REQ-028: Any text quoted as a shell word reads back unchanged. */
    #[test]
    fn shell_words_round_trip(text in "[ -~]{0,40}") {
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("printf %s {}", shell_word(&text)))
            .output()
            .unwrap();
        prop_assert_eq!(String::from_utf8(out.stdout).unwrap(), text);
    }
}

fn session(id: &str, pid: u32, kind: &str) -> crate::discovery::Session {
    crate::discovery::Session {
        session_id: id.into(),
        agent: crate::domain::Provider::Codex,
        project: "mono".into(),
        cwd: "/w/mono".into(),
        name: id.into(),
        status: "open".into(),
        kind: kind.into(),
        host: None,
        pid,
        socket: None,
    }
}

/* REQ-028: A started Codex session gets its name through the launched
process (itself or an ancestor of the session's process); the name stays with
the session id, and subagents and other sessions keep theirs. */
#[test]
fn binds_launched_processes_to_their_sessions() {
    let mut names = SessionNames::default();
    names.pids.insert(
        100,
        Launched {
            name: "mono-qa-1".into(),
            claimed_at: 1_000,
        },
    );
    names.pids.insert(
        500,
        Launched {
            name: "mono-old-1".into(),
            claimed_at: 1_000 - BIND_WINDOW_SECS,
        },
    );
    let parents = BTreeMap::from([(300, 200), (200, 100), (100, 1)]);
    let mut sessions = vec![
        session("codex:sub", 300, "subagent"),
        session("codex:other", 900, "session"),
        session("codex:spawned", 300, "session"),
    ];
    assert!(names.apply(&mut sessions, &parents, 1_000));
    assert_eq!(sessions[0].name, "codex:sub");
    assert_eq!(sessions[1].name, "codex:other");
    assert_eq!(sessions[2].name, "mono-qa-1");
    assert_eq!(names.pids.keys().collect::<Vec<_>>(), [&500]);
    let mut again = vec![session("codex:spawned", 4242, "session")];
    assert!(!names.apply(&mut again, &BTreeMap::new(), 1_001));
    // Past the window a session under the launched process takes no name,
    // while the launched process itself still does (it may wait at a prompt).
    let under = BTreeMap::from([(600, 500)]);
    let mut child = vec![session("codex:child", 600, "session")];
    assert!(!names.apply(&mut child, &under, 1_002));
    assert_eq!(child[0].name, "codex:child");
    let mut late = vec![session("codex:late", 500, "session")];
    assert!(names.apply(&mut late, &BTreeMap::new(), 1_003));
    assert_eq!(late[0].name, "mono-old-1");
    assert!(names.pids.is_empty());
    assert_eq!(again[0].name, "mono-qa-1");
    let saved = serde_json::to_string(&names).unwrap();
    assert!(!saved.contains("pids"), "{saved}");
}

/* REQ-028: Root acts through the session the user gave it (by id) or the
session started under that name; another session taking the name does not. */
#[test]
fn root_follows_the_session_not_its_name() {
    let book = crate::roles::RoleBook::default().with_root();
    let given = book
        .assign_session("claude:a", "boss", Some("root"))
        .unwrap();
    let names = SessionNames::default();
    assert!(holds_root(&given, &names, "claude:a", "boss"));
    assert!(!holds_root(&given, &names, "claude:impostor", "boss"));
    assert!(!holds_root(&given, &names, "claude:a", "renamed"));
    let taken = given.assign_session("claude:a", "boss", None).unwrap();
    assert!(taken.root_sessions.is_empty());
    assert!(!holds_root(&taken, &names, "claude:a", "boss"));
    let spawned = book.assign("mono-root-1", Some("root")).unwrap();
    let mut names = SessionNames::default();
    names
        .sessions
        .insert("codex:s".into(), "mono-root-1".into());
    assert!(holds_root(&spawned, &names, "codex:s", "mono-root-1"));
    assert!(!holds_root(&spawned, &names, "claude:other", "mono-root-1"));
}
