use super::*;
use std::os::unix::fs::PermissionsExt;

fn folder() -> (tempfile::TempDir, RoleFolder) {
    let base = tempfile::tempdir().unwrap();
    fs::set_permissions(base.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let folder = RoleFolder::open(base.path().join("roles")).unwrap();
    (base, folder)
}

fn names(book: &RoleBook) -> Vec<&str> {
    book.roles.iter().map(|r| r.name.as_str()).collect()
}

/* REQ-037: On start, roles still kept in roles.json become files (a file
that is already there is never replaced), roles.json then keeps only the
assignments, the root sessions and the presets offered, and the folder is
private. */
#[test]
fn migrates_roles_json_to_files() {
    let (_base, folder) = folder();
    // As roles.json looks in 0.4.4.
    let legacy: RoleBook = serde_json::from_str(
        r#"{"roles": [
              {"name": "master", "prompt": "Lead the team."},
              {"name": "qa", "prompt": "Test it."},
              {"name": "root", "prompt": "Coordinate."}],
            "assignments": {"mono-qa-1": "qa"},
            "root_sessions": ["claude:1"],
            "presets": ["master", "researcher"]}"#,
    )
    .unwrap();
    fs::write(folder.path("qa"), "Edited in an editor.").unwrap();
    let mut book = legacy;
    let (scan, _, changed) = load(&folder, &mut book).unwrap();
    assert!(changed, "roles.json must be saved without its roles");
    assert_eq!(names(&book), ["master", "qa", "root"]);
    assert_eq!(book.role("qa").unwrap().prompt, "Edited in an editor.");
    assert_eq!(
        fs::read_to_string(folder.path("master")).unwrap(),
        "Lead the team."
    );
    assert_eq!(
        fs::read_to_string(folder.path("root")).unwrap(),
        "Coordinate."
    );
    assert!(
        !folder.exists("researcher"),
        "a preset offered before stays deleted"
    );
    assert!(scan.view.not_loaded.is_empty());
    let saved = serde_json::to_value(&book).unwrap();
    assert!(saved.get("roles").is_none(), "{saved}");
    assert_eq!(saved["assignments"]["mono-qa-1"], "qa");
    assert_eq!(saved["root_sessions"][0], "claude:1");
    let mode = fs::metadata(folder.dir()).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
    let mode = fs::metadata(folder.path("master"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);

    // A second start finds nothing to migrate.
    let mut again: RoleBook = serde_json::from_value(saved).unwrap();
    let (_, _, changed) = load(&folder, &mut again).unwrap();
    assert!(!changed);
    assert_eq!(names(&again), ["master", "qa", "root"]);
}

/* REQ-037: A new bridge writes the preset roles and root as files, once. */
#[test]
fn starts_with_preset_and_root_files() {
    let (_base, folder) = folder();
    let mut book = RoleBook::default();
    let (_, _, changed) = load(&folder, &mut book).unwrap();
    assert!(changed);
    assert_eq!(names(&book), ["master", "researcher", "root"]);
    for name in ["master", "researcher", "root"] {
        assert!(folder.exists(name), "{name}");
    }
    assert_eq!(
        fs::read_to_string(folder.path("root")).unwrap(),
        crate::spawn::ROOT_PROMPT
    );
}

/* REQ-037: Files that are not roles are listed with the reason and never
loaded; hidden and non-Markdown files are ignored; a link is never followed;
an invalid root.md is kept as it is and the built-in root prompt is used. */
#[test]
fn lists_files_that_are_not_roles() {
    let (_base, folder) = folder();
    let dir = folder.dir().to_owned();
    fs::write(dir.join("good.md"), "Do good work.").unwrap();
    fs::write(dir.join("empty.md"), "  \n").unwrap();
    fs::write(dir.join("big.md"), "x".repeat(16_001)).unwrap();
    fs::write(dir.join("nul.md"), "a\0b").unwrap();
    fs::write(dir.join("latin.md"), [0xff, 0xfe, 0x41]).unwrap();
    fs::write(dir.join("two words.md"), "Bad name.").unwrap();
    fs::write(dir.join("root.md"), "").unwrap();
    fs::write(dir.join(".good.md.swp"), "editor").unwrap();
    fs::write(dir.join("notes.txt"), "not a role").unwrap();
    fs::create_dir(dir.join("folder.md")).unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    fs::write(outside.path(), "Outside the folder.").unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.join("linked.md")).unwrap();

    let mut book = RoleBook {
        presets: ["master".into(), "researcher".into()].into(),
        ..RoleBook::default()
    };
    load(&folder, &mut book).unwrap();
    assert_eq!(names(&book), ["good", "root"]);
    assert_eq!(book.role("root").unwrap().prompt, crate::spawn::ROOT_PROMPT);
    assert_eq!(
        fs::read_to_string(dir.join("root.md")).unwrap(),
        "",
        "kept as the user left it"
    );
    let reasons: BTreeMap<&str, &str> = book
        .files
        .not_loaded
        .iter()
        .map(|n| (n.file.as_str(), n.reason.as_str()))
        .collect();
    assert_eq!(
        reasons.keys().copied().collect::<Vec<_>>(),
        [
            "big.md",
            "empty.md",
            "folder.md",
            "latin.md",
            "linked.md",
            "nul.md",
            "root.md",
            "two words.md"
        ]
    );
    assert!(reasons["big.md"].starts_with("empty, over 16000 bytes"));
    assert!(reasons["linked.md"].contains("symbolic link"));
    assert!(reasons["folder.md"].contains("not a regular file"));
    assert!(reasons["latin.md"].contains("UTF-8"));
    assert!(reasons["two words.md"].contains("not a role name"));
    assert_eq!(
        book.files.versions.get("root"),
        None,
        "the built-in prompt has no file version"
    );
}

/* REQ-037: A poll reads only what changed, picks up new, edited and deleted
files, writes root.md again when it is deleted, and leaves the book alone
when nothing changed. */
#[test]
fn polls_the_folder_for_changes() {
    let (_base, folder) = folder();
    let mut book = RoleBook::default();
    let (scan, mut reads, _) = load(&folder, &mut book).unwrap();
    let book = Arc::new(AsyncMutex::new(book));
    assert!(poll(&folder, &book, &scan, &mut reads).unwrap().is_none());

    fs::write(folder.path("qa"), "Test it.").unwrap();
    let scan = poll(&folder, &book, &scan, &mut reads).unwrap().unwrap();
    assert_eq!(book.blocking_lock().role("qa").unwrap().prompt, "Test it.");

    fs::write(folder.path("qa"), "Test it twice.").unwrap();
    let scan = poll(&folder, &book, &scan, &mut reads).unwrap().unwrap();
    let qa = book.blocking_lock().files.versions["qa"].clone();
    assert_eq!(qa, roles::version(b"Test it twice."));
    assert_eq!(
        book.blocking_lock().role("qa").unwrap().prompt,
        "Test it twice."
    );

    // A poll that read the folder but did not apply it (a change landed
    // meanwhile) leaves the next poll every other role.
    fs::write(folder.path("qa"), "Test it three times.").unwrap();
    let _not_applied = folder.scan(folder.list().unwrap(), &scan, &mut reads);
    fs::write(folder.path("qa"), "Test it four times, longer.").unwrap();
    let scan = poll(&folder, &book, &scan, &mut reads).unwrap().unwrap();
    assert_eq!(
        names(&book.blocking_lock()),
        ["master", "qa", "researcher", "root"]
    );
    assert_eq!(
        book.blocking_lock().role("qa").unwrap().prompt,
        "Test it four times, longer."
    );

    fs::remove_file(folder.path("qa")).unwrap();
    let scan = poll(&folder, &book, &scan, &mut reads).unwrap().unwrap();
    assert!(book.blocking_lock().role("qa").is_none());

    fs::remove_file(folder.path("root")).unwrap();
    assert!(poll(&folder, &book, &scan, &mut reads).unwrap().is_none());
    assert_eq!(
        fs::read_to_string(folder.path("root")).unwrap(),
        crate::spawn::ROOT_PROMPT
    );
    assert!(book.blocking_lock().role("root").is_some());
}

/* REQ-037: Saving from the page renames a role's file, also when only the
case of its name changes, and the version is that of the bytes on disk. */
#[test]
fn renames_and_versions_role_files() {
    let (_base, folder) = folder();
    let qa = Role {
        name: "qa".into(),
        prompt: "Test it.".into(),
    };
    folder.write(&qa).unwrap();
    assert_eq!(
        folder.version_on_disk("qa").unwrap(),
        Some(roles::version(b"Test it."))
    );
    let upper = Role {
        name: "QA".into(),
        prompt: "Test it.".into(),
    };
    folder.rename("qa", &upper).unwrap();
    let files: Vec<String> = folder.list().unwrap().into_keys().collect();
    assert_eq!(files, ["QA.md"]);
    let tester = Role {
        name: "tester".into(),
        prompt: "Test more.".into(),
    };
    folder.rename("QA", &tester).unwrap();
    let files: Vec<String> = folder.list().unwrap().into_keys().collect();
    assert_eq!(files, ["tester.md"]);
    assert_eq!(folder.version_on_disk("gone").unwrap(), None);
}
