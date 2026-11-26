use super::*;
use proptest::prelude::*;
use std::path::PathBuf;

fn schedule(id: u128, cron: &str) -> Schedule {
    Schedule {
        id: Uuid::from_u128(id),
        cron: cron.into(),
        project: "mono".into(),
        to: Some("master".into()),
        message: "Daily status, please".into(),
    }
}
const COMMAND: &str = "'/Users/me/.local/bin/session-bridge'";

/* REQ-025: The bridge recognizes only its own crontab lines. */
#[test]
fn parses_managed_lines() {
    let id = Uuid::from_u128(7);
    let text = format!(
        "MAILTO=me\n0 9 * * 1-5 /usr/bin/backup # nightly\n{HEADER}\n*/30 * * * * {COMMAND} scheduled {id} {MARKER}\n\n"
    );
    assert_eq!(
        parse(&text),
        [
            Line::Other("MAILTO=me".into()),
            Line::Other("0 9 * * 1-5 /usr/bin/backup # nightly".into()),
            Line::Other(HEADER.into()),
            Line::Managed(id),
            Line::Other(String::new()),
        ]
    );
    assert_eq!(
        parse(&format!("* * * * * echo scheduled {id}")),
        [Line::Other(format!("* * * * * echo scheduled {id}"))],
        "without the marker a line is the user's"
    );
}

/* REQ-025: Rewriting keeps every other line byte for byte and in order, and
replaces the managed lines with the current schedules. */
#[test]
fn renders_managed_lines_after_the_users() {
    let old = Uuid::from_u128(1);
    let text = format!(
        "MAILTO=me\n{HEADER}\n0 1 * * * {COMMAND} scheduled {old} {MARKER}\n15 3 * * *  /usr/bin/backup   --all\n"
    );
    let rendered = render(&text, &[schedule(2, "0 9 * * 1-5")], COMMAND, &[old].into());
    assert_eq!(
        rendered,
        format!(
            "MAILTO=me\n15 3 * * *  /usr/bin/backup   --all\n{HEADER}\n0 9 * * 1-5 {COMMAND} scheduled {} {MARKER}\n",
            Uuid::from_u128(2)
        )
    );
    assert_eq!(
        render(&text, &[], COMMAND, &[old].into()),
        "MAILTO=me\n15 3 * * *  /usr/bin/backup   --all\n"
    );
    assert_eq!(render("", &[], COMMAND, &Default::default()), "");
    assert_eq!(
        render("", &[schedule(3, "@daily")], COMMAND, &Default::default()),
        format!(
            "{HEADER}\n@daily {COMMAND} scheduled {} {MARKER}\n",
            Uuid::from_u128(3)
        )
    );
}

/* REQ-025: A rewrite removes only this bridge's own managed lines; another
bridge's lines and Windows line endings stay exactly as they were. */
#[test]
fn keeps_other_bridges_lines_and_line_endings() {
    let foreign = Uuid::from_u128(11);
    let mine = Uuid::from_u128(1);
    let text = format!(
        "MAILTO=me\r\n0 2 * * * '/other/session-bridge' scheduled {foreign} {MARKER}\n0 1 * * * {COMMAND} scheduled {mine} {MARKER}\n# trailing  \n"
    );
    let owned = std::collections::BTreeSet::from([mine]);
    let rendered = render(&text, &[schedule(2, "@daily")], COMMAND, &owned);
    assert_eq!(
        rendered,
        format!(
            "MAILTO=me\r\n0 2 * * * '/other/session-bridge' scheduled {foreign} {MARKER}\n# trailing  \n{HEADER}\n@daily {COMMAND} scheduled {} {MARKER}\n",
            Uuid::from_u128(2)
        )
    );
    assert_eq!(
        foreign_lines(&text, &owned),
        [format!(
            "0 2 * * * '/other/session-bridge' scheduled {foreign} {MARKER}"
        )]
    );
}

/* REQ-025: A schedule needs a valid cron expression (no %, which cron turns
into a newline), a project name, a plain recipient and a valid message. */
#[test]
fn validates_schedules() {
    for cron in [
        "*/15 * * * *",
        "0 9 * * mon-fri",
        "5,35 8-18 1 jan *",
        "@hourly",
        "@daily",
        "@weekly",
        "@monthly",
        "@yearly",
        "@reboot",
    ] {
        assert_eq!(validate(&schedule(1, cron)), Ok(()), "{cron}");
    }
    for cron in [
        "* * * *",
        "* * * * * *",
        "0 9 * * 1%2",
        "@sometimes",
        "* * * * ;rm",
        "0 9\n* * *",
        "",
    ] {
        assert_eq!(
            validate(&schedule(1, cron)),
            Err(ScheduleError::Cron),
            "{cron:?}"
        );
    }
    let mut bad = schedule(1, "@daily");
    bad.project = "/work/mono".into();
    assert_eq!(validate(&bad), Err(ScheduleError::Project));
    let mut bad = schedule(1, "@daily");
    bad.to = Some("two words".into());
    assert_eq!(validate(&bad), Err(ScheduleError::Recipient));
    for to in ["bob-", "bob.", "-", ""] {
        let mut bad = schedule(1, "@daily");
        bad.to = Some(to.into());
        assert_eq!(validate(&bad), Err(ScheduleError::Recipient), "{to}");
    }
    let mut bad = schedule(1, "@daily");
    bad.message = " ".into();
    assert_eq!(validate(&bad), Err(ScheduleError::Message));
    let mut ok = schedule(1, "@daily");
    ok.to = None;
    assert_eq!(validate(&ok), Ok(()));
    ok.to = Some("channel".into());
    assert_eq!(validate(&ok), Ok(()));
}

/* REQ-025: The cron command quotes the executable for the shell and names a
separate bridge's state directory. */
#[test]
fn quotes_the_command() {
    assert_eq!(
        command(&PathBuf::from("/Users/me/.local/bin/session-bridge"), None),
        "'/Users/me/.local/bin/session-bridge'"
    );
    assert_eq!(
        command(
            &PathBuf::from("/tmp/it's/session-bridge"),
            Some(&PathBuf::from("/tmp/a b"))
        ),
        "SESSION_BRIDGE_STATE='/tmp/a b' '/tmp/it'\\''s/session-bridge'"
    );
    // cron turns an unescaped % into a newline.
    assert_eq!(
        command(&PathBuf::from("/tmp/100%/session-bridge"), None),
        "'/tmp/100\\%/session-bridge'"
    );
}

proptest! {
    /* REQ-025: Any user lines (without the marker) survive a rewrite unchanged and in order. */
    #[test]
    fn user_lines_survive(lines in prop::collection::vec("[ -~]{0,40}", 0..12), count in 0u128..3) {
        let lines: Vec<String> = lines.into_iter().filter(|l| !l.contains("session-bridge")).collect();
        let text = lines.iter().map(|l| format!("{l}\n")).collect::<String>();
        let schedules: Vec<Schedule> = (0..count).map(|i| schedule(i, "@daily")).collect();
        let rendered = render(&text, &schedules, COMMAND, &Default::default());
        let kept: Vec<Line> = parse(&rendered).into_iter().filter(|l| matches!(l, Line::Other(t) if t != HEADER)).collect();
        prop_assert_eq!(kept, lines.into_iter().map(Line::Other).collect::<Vec<_>>());
        let managed = parse(&rendered).into_iter().filter(|l| matches!(l, Line::Managed(_))).count();
        prop_assert_eq!(managed as u128, count);
    }
}
