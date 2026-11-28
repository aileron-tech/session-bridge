use super::*;
use crate::domain::Provider;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

fn session(id: &str, name: &str, dir: &str, host: Option<&str>) -> Session {
    Session {
        session_id: id.into(),
        agent: Provider::Claude,
        project: crate::discovery::project_name(Path::new(dir)),
        cwd: dir.into(),
        name: name.into(),
        status: "idle".into(),
        kind: "session".into(),
        host: host.map(str::to_owned),
        pid: 1,
        socket: None,
    }
}
fn m<'a>(name: &'a str, role: Option<&'a str>) -> Member<'a> {
    Member { name, role }
}

/* REQ-030: The master is the user's live choice, else a session named or
holding master, else lead, else the first by name. */
#[test]
fn chooses_the_master() {
    let team = [
        m("zed", None),
        m("shop-lead-1", None),
        m("bob", Some("Master")),
    ];
    assert_eq!(master(&team, None), Some(("bob".into(), false)));
    assert_eq!(master(&team, Some("ZED")), Some(("zed".into(), true)));
    assert_eq!(master(&team, Some("gone")), Some(("bob".into(), false)));
    let leads = [m("zed", None), m("shop-lead-1", None)];
    assert_eq!(master(&leads, None), Some(("shop-lead-1".into(), false)));
    let plain = [m("zed", None), m("amy", Some("qa"))];
    assert_eq!(master(&plain, None), Some(("amy".into(), false)));
    assert_eq!(master(&[], Some("x")), None);
}

/* REQ-030: Projects are this laptop's directories with live sessions; the
same folder name elsewhere is another project, and paired laptops' sessions
are left out. */
#[test]
fn groups_local_sessions_by_directory() {
    let sessions = [
        session("c:1", "shop-qa-1", "/w/shop", None),
        session("c:2", "master", "/w/shop", None),
        session("c:3", "other", "/x/shop", None),
        session("c:4", "far", "/w/shop", Some("laptop-b")),
    ];
    let chosen = BTreeMap::from([("shop".to_owned(), "shop-qa-1".to_owned())]);
    let views = projects(
        &sessions,
        &BTreeSet::new(),
        &BTreeSet::new(),
        &RoleBook::default(),
        &chosen,
    );
    assert_eq!(views.len(), 2);
    assert_eq!(views[0].dir, Path::new("/w/shop"));
    assert_eq!(views[0].sessions, ["c:2", "c:1"]);
    assert_eq!(views[0].master.as_deref(), Some("shop-qa-1"));
    assert!(views[0].master_chosen);
    assert_eq!(views[1].master.as_deref(), Some("other"));
    assert!(!views[1].master_chosen);
    let auto = projects(
        &sessions,
        &BTreeSet::new(),
        &BTreeSet::new(),
        &RoleBook::default(),
        &BTreeMap::new(),
    );
    assert_eq!(auto[0].master.as_deref(), Some("master"));
}

/* REQ-038: The Projects tab keeps a project after its sessions end: a
remembered directory without live sessions is listed with none, once, after
the projects with live sessions (REQ-040). */
#[test]
fn keeps_remembered_projects_without_live_sessions() {
    let sessions = [session("c:1", "dev", "/w/shop", None)];
    let remembered = BTreeSet::from([PathBuf::from("/w/shop"), PathBuf::from("/a/old")]);
    let chosen = BTreeMap::from([("old".to_owned(), "gone".to_owned())]);
    let views = projects(
        &sessions,
        &remembered,
        &BTreeSet::new(),
        &RoleBook::default(),
        &chosen,
    );
    assert_eq!(views.len(), 2);
    assert_eq!(views[0].sessions, ["c:1"]);
    assert_eq!(views[1].project, "old");
    assert_eq!(views[1].dir, Path::new("/a/old"));
    assert!(views[1].sessions.is_empty());
    assert_eq!(views[1].master, None);
    assert!(!views[1].master_chosen);
}

/* REQ-040: Projects are ordered starred first, then those with live
sessions, then by project name, then by folder; a star on a folder that is
not listed adds no project. */
#[test]
fn orders_starred_then_live_then_by_name_and_folder() {
    let sessions = [
        session("c:1", "dev", "/w/zoo", None),
        session("c:2", "dev", "/x/app", None),
        session("c:3", "dev", "/w/app", None),
    ];
    let remembered = BTreeSet::from([
        PathBuf::from("/w/old"),
        PathBuf::from("/w/kept"),
        PathBuf::from("/a/bar"),
    ]);
    let stars = BTreeSet::from([
        PathBuf::from("/w/kept"),
        PathBuf::from("/w/zoo"),
        PathBuf::from("/w/gone"),
    ]);
    let views = projects(
        &sessions,
        &remembered,
        &stars,
        &RoleBook::default(),
        &BTreeMap::new(),
    );
    let order: Vec<(&Path, bool)> = views.iter().map(|v| (v.dir.as_path(), v.starred)).collect();
    assert_eq!(
        order,
        [
            (Path::new("/w/zoo"), true),
            (Path::new("/w/kept"), true),
            (Path::new("/w/app"), false),
            (Path::new("/x/app"), false),
            (Path::new("/a/bar"), false),
            (Path::new("/w/old"), false),
        ]
    );
}

/* REQ-040: A star is kept for an absolute folder, listed or not, and removed
on unstar; a relative folder is refused and the stars stay as they were. */
#[test]
fn stars_and_unstars_absolute_folders() {
    let none = BTreeSet::new();
    let one = star(&none, Path::new("/w/shop"), true).unwrap();
    assert_eq!(one, BTreeSet::from([PathBuf::from("/w/shop")]));
    assert_eq!(star(&one, Path::new("/w/shop"), true).unwrap(), one);
    let two = star(&one, Path::new("/not/listed"), true).unwrap();
    assert_eq!(two.len(), 2);
    assert_eq!(star(&two, Path::new("/not/listed"), false).unwrap(), one);
    assert_eq!(star(&none, Path::new("/w/shop"), false).unwrap(), none);
    assert_eq!(
        star(&one, Path::new("w/shop"), true),
        Err(StarError::NotAbsolute)
    );
    assert_eq!(
        star(&one, Path::new(""), false),
        Err(StarError::NotAbsolute)
    );
}

/* REQ-038: Remembered projects are the directories of this laptop's own
sessions: paired laptops' sessions, subagents and the user are left out. */
#[test]
fn remembers_only_local_session_directories() {
    let mut sub = session("c:3", "sub", "/w/sub", None);
    sub.kind = "subagent".into();
    let mut user = session("user:1", "user", "/w/user", None);
    user.agent = Provider::User;
    let known = [
        session("c:1", "dev", "/w/shop", None),
        session("c:2", "qa", "/w/shop", None),
        session("c:4", "far", "/w/far", Some("laptop-b")),
        sub,
        user,
    ];
    assert_eq!(
        remembered(&known),
        BTreeSet::from([PathBuf::from("/w/shop")])
    );
}

/* REQ-038: A folder under a system temp root is scratch, not a project to
remember, unless it is inside the bridge's own home in any of its spellings. */
#[test]
fn treats_temp_folders_outside_home_as_scratch() {
    let roots = [
        PathBuf::from("/tmp"),
        PathBuf::from("/private/tmp"),
        PathBuf::from("/private/var/folders"),
    ];
    let home = [PathBuf::from("/Users/me")];
    assert!(is_scratch(Path::new("/tmp/bench/run"), &roots, &home));
    assert!(is_scratch(
        Path::new("/private/var/folders/x/T/a"),
        &roots,
        &home
    ));
    assert!(!is_scratch(Path::new("/Users/me/work/shop"), &roots, &home));
    assert!(!is_scratch(Path::new("/tmpx/shop"), &roots, &home));
    let test_home = [
        PathBuf::from("/tmp/sbqa-1/home"),
        PathBuf::from("/private/tmp/sbqa-1/home"),
    ];
    assert!(!is_scratch(
        Path::new("/private/tmp/sbqa-1/home/work/p"),
        &roots,
        &test_home
    ));
    assert!(is_scratch(
        Path::new("/private/tmp/other"),
        &roots,
        &test_home
    ));
}
