use super::*;

fn member(name: &str, last: Option<LastMessage>) -> TeamRow {
    TeamRow {
        name: name.into(),
        role: Some("dev".into()),
        agent: "codex".into(),
        host: "this laptop".into(),
        status: "idle".into(),
        usage: Some("CPU 3%, memory 400 MiB, 1 process".into()),
        last,
    }
}

/* REQ-031: A kickoff carries a Markdown table of the project's sessions:
role, agent, host, state, usage and the last message (who, when, the first
80 characters), on one line per session, column breaks escaped. */
#[test]
fn builds_the_team_table() {
    let long = format!("line one\nline | two {}", "x".repeat(200));
    let rows = vec![
        member(
            "mono-dev-1",
            Some(LastMessage {
                at: 1_000,
                sent: true,
                text: long,
            }),
        ),
        TeamRow {
            role: None,
            usage: None,
            host: "laptop-b".into(),
            status: "busy".into(),
            last: None,
            ..member("qa", None)
        },
    ];
    let text = attach("Kick off: report.", "mono", &rows, 1_000 + 300, 16_000);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "Kick off: report.");
    assert_eq!(lines[2], "Team in #mono at send time:");
    assert_eq!(
        lines[4],
        "| Session | Role | Agent | Host | State | Usage | Last message |"
    );
    assert!(
        lines[6].starts_with(
            "| mono-dev-1 | dev | codex | this laptop | idle | CPU 3%, memory 400 MiB, 1 process | sent 5 min ago: line one line \\| two xxx"
        ),
        "{}",
        lines[6]
    );
    assert!(lines[6].ends_with("... |"), "{}", lines[6]);
    assert_eq!(lines[7], "| qa | - | codex | laptop-b | busy | - | - |");
    assert_eq!(lines.len(), 8);
}

/* REQ-031: The message with its table stays within the message limit; rows
that do not fit are counted instead. */
#[test]
fn keeps_within_the_limit() {
    let rows: Vec<TeamRow> = (0..400).map(|i| member(&format!("s{i}"), None)).collect();
    let text = attach("Kick off.", "mono", &rows, 0, 16_000);
    assert!(text.len() <= 16_000, "{}", text.len());
    assert!(
        text.ends_with("more sessions left out)"),
        "{}",
        &text[text.len() - 60..]
    );
    let empty = attach("Kick off.", "mono", &[], 0, 16_000);
    assert!(empty.ends_with("| - | - | - | - | - | - | - |"));
}
