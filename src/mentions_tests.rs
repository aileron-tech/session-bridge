use super::*;
use proptest::prelude::*;

fn member(id: &str, name: &str) -> Member {
    Member {
        session_id: id.into(),
        name: name.into(),
        role: None,
    }
}
fn with_role(mut member: Member, role: &str) -> Member {
    member.role = Some(role.into());
    member
}
fn team() -> Vec<Member> {
    vec![
        member("claude:1", "mono-opus-dev1"),
        member("claude:2", "mono-opus-qa1"),
        member("codex:3", "master"),
    ]
}

/* REQ-020: Mentions are @-words at a word start; emails and trailing punctuation are not part of them. */
#[test]
fn finds_mentions() {
    assert_eq!(
        mentioned("@channel please, and @mono-opus-dev1. Ask me@example.com (@Master)"),
        ["channel", "mono-opus-dev1", "Master"]
    );
    assert_eq!(mentioned("no mentions @ here"), Vec::<String>::new());
    assert_eq!(mentioned("@a_b.c-"), ["a_b.c"]);
}

/* REQ-020: @channel reaches every session; @name reaches that session; case does not matter. */
#[test]
fn resolves_channel_and_names() {
    assert_eq!(
        resolve("@channel stop", &team(), "mono", None).unwrap(),
        ["claude:1", "claude:2", "codex:3"]
    );
    assert_eq!(
        resolve(
            "@MASTER and @mono-opus-qa1, also @master",
            &team(),
            "mono",
            None
        )
        .unwrap(),
        ["claude:2", "codex:3"]
    );
    assert_eq!(
        resolve("@channel and @master", &team(), "mono", None).unwrap(),
        ["claude:1", "claude:2", "codex:3"]
    );
}

/* REQ-024: Without a mention a post goes to the channel's default recipient;
any mention overrides it; a default that is not live is refused. */
#[test]
fn uses_the_default_recipient() {
    assert_eq!(
        resolve("status please", &team(), "mono", Some("master")).unwrap(),
        ["codex:3"]
    );
    assert_eq!(
        resolve("@mono-opus-qa1 only you", &team(), "mono", Some("master")).unwrap(),
        ["claude:2"]
    );
    assert_eq!(
        resolve("@channel all", &team(), "mono", Some("master")).unwrap(),
        ["claude:1", "claude:2", "codex:3"]
    );
    assert_eq!(
        resolve("status please", &team(), "mono", Some("lead")),
        Err(MentionError::DefaultGone {
            name: "lead".into(),
            channel: "mono".into()
        })
    );
    assert_eq!(
        MentionError::DefaultGone {
            name: "lead".into(),
            channel: "mono".into()
        }
        .to_string(),
        "The default recipient @lead is not live in #mono; mention someone or change the default"
    );
}

/* REQ-026: @role reaches every session with that role; a session name wins
over a role of the same name; a role can be the channel default. */
#[test]
fn resolves_roles() {
    let team = vec![
        with_role(member("claude:1", "dev1"), "reviewer"),
        with_role(member("claude:2", "dev2"), "Reviewer"),
        with_role(member("codex:3", "master"), "lead"),
        member("claude:4", "lead"),
    ];
    assert_eq!(
        resolve("@reviewer please look", &team, "mono", None).unwrap(),
        ["claude:1", "claude:2"]
    );
    assert_eq!(
        resolve("@lead status", &team, "mono", None).unwrap(),
        ["claude:4"],
        "the session named lead, not the role"
    );
    assert_eq!(
        resolve("status", &team, "mono", Some("reviewer")).unwrap(),
        ["claude:1", "claude:2"]
    );
    assert_eq!(
        resolve("@tester go", &team, "mono", None),
        Err(MentionError::Unknown {
            name: "tester".into(),
            channel: "mono".into()
        })
    );
}

/* REQ-020: A post without a mention, to an unknown or ambiguous name, or to an empty channel is refused. */
#[test]
fn refuses_unclear_posts() {
    assert_eq!(
        resolve("hello all", &team(), "mono", None),
        Err(MentionError::NoMention("mono".into()))
    );
    assert_eq!(
        resolve("@nobody hi", &team(), "mono", None),
        Err(MentionError::Unknown {
            name: "nobody".into(),
            channel: "mono".into()
        })
    );
    let twins = vec![member("claude:1", "dev"), member("claude:2", "Dev")];
    assert_eq!(
        resolve("@dev hi", &twins, "mono", None),
        Err(MentionError::Ambiguous {
            name: "dev".into(),
            channel: "mono".into()
        })
    );
    assert_eq!(
        resolve("@channel hi", &[], "mono", None),
        Err(MentionError::EmptyChannel("mono".into()))
    );
    assert_eq!(
        MentionError::NoMention("mono".into()).to_string(),
        "Mention @channel or a session (@name) to post in #mono"
    );
}

proptest! {
    /* REQ-020: Any text resolves to a subset of the channel's sessions, without duplicates, or to an error. */
    #[test]
    fn resolves_to_members_only(text in "(?s).{0,200}", names in prop::collection::vec("[a-z]{1,4}", 0..5)) {
        let members: Vec<Member> = names
            .iter()
            .enumerate()
            .map(|(i, n)| member(&format!("claude:{i}"), n))
            .collect();
        if let Ok(ids) = resolve(&text, &members, "p", None) {
            prop_assert!(!ids.is_empty());
            let mut unique = ids.clone();
            unique.dedup();
            prop_assert_eq!(&unique, &ids);
            prop_assert!(ids.iter().all(|id| members.iter().any(|m| &m.session_id == id)));
        }
    }

    /* REQ-020: Every member named with @ in otherwise plain text is reached. */
    #[test]
    fn named_members_are_reached(pick in 0usize..3, filler in "[a-z ]{0,20}") {
        let target = &team()[pick];
        let text = format!("{filler} @{} {filler}", target.name);
        prop_assert_eq!(resolve(&text, &team(), "mono", None).unwrap(), vec![target.session_id.clone()]);
    }
}
