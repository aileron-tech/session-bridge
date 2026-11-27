use super::*;

fn role(name: &str) -> Role {
    Role {
        name: name.into(),
        prompt: format!("You are the {name}. Reply with blockers first."),
    }
}

/* REQ-026: Roles are added, renamed (with their assignments) and deleted
(with their assignments); names are mention-safe and unique. */
#[test]
fn keeps_roles_and_assignments() {
    let book = RoleBook::default().save(role("reviewer"), None).unwrap();
    let book = book.assign("dev1", Some("REVIEWER")).unwrap();
    assert_eq!(book.role_of("dev1"), Some("reviewer"));
    assert_eq!(book.role("Reviewer").unwrap().name, "reviewer");
    assert_eq!(
        book.save(role("reviewer"), None),
        Err(RoleError::Exists("reviewer".into()))
    );
    let renamed = book.save(role("qa"), Some("reviewer")).unwrap();
    assert_eq!(renamed.roles, [role("qa")]);
    assert_eq!(renamed.role_of("dev1"), Some("qa"));
    let deleted = renamed.delete("qa").unwrap();
    assert!(deleted.roles.is_empty());
    assert_eq!(deleted.role_of("dev1"), None);
    assert_eq!(deleted.delete("qa"), Err(RoleError::Unknown("qa".into())));
    for bad in ["", "two words", "channel", "Channel", "lead-", "lead."] {
        assert_eq!(
            RoleBook::default().save(role(bad), None),
            Err(RoleError::Name),
            "{bad:?}"
        );
    }
    let empty = Role {
        name: "lead".into(),
        prompt: " ".into(),
    };
    assert_eq!(
        RoleBook::default().save(empty, None),
        Err(RoleError::Prompt)
    );
}

/* REQ-026: Assigning needs an existing role; clearing removes the badge. */
#[test]
fn assigns_and_clears() {
    let book = RoleBook::default().save(role("lead"), None).unwrap();
    assert_eq!(
        book.assign("dev1", Some("tester")),
        Err(RoleError::Unknown("tester".into()))
    );
    let book = book.assign("dev1", Some("lead")).unwrap();
    let book = book.assign("dev2", Some("lead")).unwrap();
    assert_eq!(book.role_of("dev2"), Some("lead"));
    let cleared = book.assign("dev1", None).unwrap();
    assert_eq!(cleared.role_of("dev1"), None);
    assert_eq!(cleared.role_of("dev2"), Some("lead"));
}

/* REQ-026: The assignment message names the role and carries its prompt. */
#[test]
fn writes_the_assignment_message() {
    assert_eq!(
        assignment_message(&role("reviewer")),
        "Your role in this workspace: reviewer.\n\nYou are the reviewer. Reply with blockers first.\n\nIn channels, posts that mention @reviewer reach you."
    );
}

/* REQ-028: The root role is always there; its instructions change, its name
and existence do not, and no other role takes its name. */
#[test]
fn keeps_the_builtin_root_role() {
    let book = RoleBook::default().with_root();
    assert_eq!(book.role("root").unwrap().prompt, crate::spawn::ROOT_PROMPT);
    let added = book.save(role("qa"), None).unwrap();
    assert_eq!(added.roles.last().unwrap().name, "root", "root stays last");
    assert_eq!(book.clone().with_root(), book);
    assert_eq!(book.delete("Root"), Err(RoleError::Builtin));
    assert_eq!(
        book.save(role("admin"), Some("root")),
        Err(RoleError::Builtin)
    );
    assert_eq!(
        book.save(role("root"), None),
        Err(RoleError::Exists("root".into()))
    );
    let changed = book
        .save(
            Role {
                name: "root".into(),
                prompt: "Coordinate.".into(),
            },
            Some("root"),
        )
        .unwrap();
    assert_eq!(changed.role("root").unwrap().prompt, "Coordinate.");
    let assigned = changed.assign("mono-root-1", Some("root")).unwrap();
    assert_eq!(assigned.role_of("mono-root-1"), Some("root"));
}

/* REQ-026: The preset roles master and researcher are offered once, before
root, with the user's texts; a role the user already has under that name
keeps its text, and a preset the user deleted is not offered again. */
#[test]
fn offers_the_preset_roles_once() {
    let book = RoleBook::default().with_root().with_presets();
    let names: Vec<&str> = book.roles.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["master", "researcher", "root"]);
    assert!(
        book.role("master")
            .unwrap()
            .prompt
            .starts_with("Multi-Agent Orchestrator")
    );
    assert!(
        book.role("researcher")
            .unwrap()
            .prompt
            .starts_with("Critical Path Research Support")
    );
    assert_eq!(book.clone().with_presets(), book);

    let deleted = book.delete("researcher").unwrap();
    let reloaded: RoleBook =
        serde_json::from_value(serde_json::to_value(&deleted).unwrap()).unwrap();
    assert!(
        reloaded
            .with_root()
            .with_presets()
            .role("researcher")
            .is_none()
    );

    let mine = RoleBook::default()
        .save(role("master"), None)
        .unwrap()
        .with_root()
        .with_presets();
    assert_eq!(mine.role("master").unwrap().prompt, role("master").prompt);
    assert!(mine.role("researcher").is_some());
    for preset in [
        crate::role_presets::MASTER_PROMPT,
        crate::role_presets::RESEARCHER_PROMPT,
    ] {
        assert!(preset.is_ascii());
        assert!(preset.len() <= 16_000);
    }
}

/* REQ-037: A role file is `<name>.md` with the prompt as is; hidden, editor
and non-Markdown files are no roles; only new or changed listings are read
again; roles are ordered by name with root last. */
#[test]
fn reads_role_files() {
    assert_eq!(file_name("qa"), "qa.md");
    assert_eq!(role_of_file("qa.md"), Some("qa"));
    for ignored in [".qa.md", "qa.md~", "qa.txt", ".DS_Store", "qa.md.swp"] {
        assert_eq!(role_of_file(ignored), None, "{ignored}");
    }
    let prompt = "# QA\n\nTest **every** change.\n";
    assert_eq!(
        role_from_file("qa", prompt.as_bytes()),
        Ok(Role {
            name: "qa".into(),
            prompt: prompt.into()
        })
    );
    assert_eq!(role_from_file("channel", b"x"), Err(FileProblem::Name));
    assert_eq!(role_from_file("qa", b" \n"), Err(FileProblem::Prompt));
    assert_eq!(role_from_file("qa", &[0xff]), Err(FileProblem::NotUtf8));
    assert_ne!(version(b"a"), version(b"b"));
    assert_eq!(version(b"a").len(), 32);

    let stamp = |len| Stamp {
        kind: FileKind::Regular,
        len,
        modified: None,
    };
    let last: Listing = [("a.md".into(), stamp(1)), ("b.md".into(), stamp(2))].into();
    let next: Listing = [
        ("a.md".into(), stamp(1)),
        ("b.md".into(), stamp(3)),
        ("c.md".into(), stamp(1)),
    ]
    .into();
    assert_eq!(
        changed_files(&last, &next).into_iter().collect::<Vec<_>>(),
        ["b.md", "c.md"]
    );

    let read =
        |file: &str, name: &str| (file.to_owned(), Ok((role(name), version(name.as_bytes()))));
    let (roles, view) = assemble(vec![
        read("root.md", "root"),
        read("Zed.md", "Zed"),
        read("alpha.md", "alpha"),
    ]);
    let names: Vec<&str> = roles.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["alpha", "Zed", "root"]);
    assert_eq!(view.versions.len(), 3);
    // Two files for one name (a case-sensitive disk): the first by file name loads.
    let (roles, view) = assemble(vec![read("QA.md", "QA"), read("qa.md", "qa")]);
    assert_eq!(roles, [role("QA")]);
    assert_eq!(view.not_loaded[0].file, "qa.md");
    assert!(
        view.not_loaded[0]
            .reason
            .contains("same role name as QA.md")
    );
}
