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
