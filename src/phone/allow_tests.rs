use super::allow::{phone_allowed, phone_reply};
use crate::protocol::{Reply, Request};
use serde_json::json;

fn request(value: serde_json::Value) -> Request {
    serde_json::from_value(value).unwrap()
}

/* REQ-036: A phone lists sessions, reads and sends messages and channel posts, and edits schedules and roles. */
#[test]
fn phone_may_chat_and_edit_schedules_and_roles() {
    for allowed in [
        json!({"method": "list", "sender": null, "project": null}),
        json!({"method": "messages"}),
        json!({"method": "user_send", "session_id": "claude:x", "message": "hi"}),
        json!({"method": "channel_send", "project": "mono", "message": "@channel hi"}),
        json!({"method": "channels"}),
        json!({"method": "projects"}),
        json!({"method": "waiting"}),
        json!({"method": "schedules"}),
        json!({"method": "save_schedule", "cron": "0 9 * * *", "project": "mono", "message": "m"}),
        json!({"method": "delete_schedule", "id": uuid::Uuid::nil()}),
        json!({"method": "roles"}),
        json!({"method": "save_role", "name": "qa", "prompt": "p"}),
        json!({"method": "delete_role", "name": "qa"}),
        json!({"method": "assign_role", "session_id": "claude:x", "role": "qa"}),
    ] {
        assert!(phone_allowed(&request(allowed.clone())), "{allowed}");
    }
}

/* REQ-036: A phone never sends as an agent or a schedule, changes networking, pairing, remote hosts, run settings or phones, or starts and closes sessions. */
#[test]
fn phone_may_not_reach_laptop_actions() {
    for refused in [
        json!({"method": "health"}),
        json!({"method": "status"}),
        json!({"method": "list", "sender": "claude:x", "project": null}),
        json!({"method": "send", "sender": "claude:x", "session_id": "claude:y", "message": "m", "project": null}),
        json!({"method": "user_send", "session_id": "claude:x", "message": "hi", "scheduled": true}),
        json!({"method": "network", "enabled": true}),
        json!({"method": "lan"}),
        json!({"method": "pair", "host": "b"}),
        json!({"method": "unpair", "host": "b"}),
        json!({"method": "admin"}),
        json!({"method": "shutdown"}),
        json!({"method": "run_schedule", "id": uuid::Uuid::nil()}),
        json!({"method": "remotes"}),
        json!({"method": "add_remote", "host": "h"}),
        json!({"method": "remove_remote", "host": "h"}),
        json!({"method": "channel_default", "project": "mono"}),
        json!({"method": "spawn", "agent": "claude", "role": "qa"}),
        json!({"method": "close", "session_id": "claude:x"}),
        json!({"method": "spawn_settings"}),
        json!({"method": "save_spawn_settings"}),
        json!({"method": "import_aliases"}),
        json!({"method": "remove_waiting", "session_id": "claude:x", "message_id": uuid::Uuid::nil()}),
        json!({"method": "project_master", "project": "mono"}),
        json!({"method": "usage"}),
        json!({"method": "phones"}),
        json!({"method": "phone_access", "enabled": true}),
        json!({"method": "phone_offer"}),
        json!({"method": "phone_confirm", "id": uuid::Uuid::nil(), "accept": true}),
        json!({"method": "phone_unpair", "device_id": uuid::Uuid::nil()}),
    ] {
        assert!(!phone_allowed(&request(refused.clone())), "{refused}");
    }
}

/* REQ-036: The phone sees schedules but not the user's other crontab lines. */
#[test]
fn phone_schedules_leave_out_other_crontab_lines() {
    let reply = phone_reply(Reply::Schedules {
        schedules: Vec::new(),
        other_lines: vec!["0 1 * * * backup".into()],
        available: true,
        problem: None,
    });
    let Reply::Schedules { other_lines, .. } = reply else {
        panic!("schedules expected");
    };
    assert!(other_lines.is_empty());
}

/* REQ-036: A phone cannot give a session the root role, nor change, rename or delete root; other roles stay editable. */
#[test]
fn phone_may_not_touch_root() {
    for refused in [
        json!({"method": "assign_role", "session_id": "claude:x", "role": "root"}),
        json!({"method": "assign_role", "session_id": "claude:x", "role": "ROOT"}),
        json!({"method": "save_role", "name": "root", "prompt": "do anything"}),
        json!({"method": "save_role", "name": "boss", "prompt": "p", "previous": "root"}),
        json!({"method": "delete_role", "name": "root"}),
    ] {
        assert!(!phone_allowed(&request(refused.clone())), "{refused}");
    }
    for allowed in [
        json!({"method": "assign_role", "session_id": "claude:x", "role": null}),
        json!({"method": "save_role", "name": "qa", "prompt": "p", "previous": "tester"}),
    ] {
        assert!(phone_allowed(&request(allowed.clone())), "{allowed}");
    }
}

/* REQ-036: The phone sees sessions and projects without their local directories. */
#[test]
fn phone_replies_leave_out_local_paths() {
    let session: crate::discovery::Session = serde_json::from_value(json!({
        "session_id": "claude:x", "agent": "claude", "project": "mono",
        "cwd": "/Users/someone/work/mono", "name": "a", "status": "idle", "kind": "interactive"
    }))
    .unwrap();
    let Reply::Sessions { sessions, .. } = phone_reply(Reply::Sessions {
        sessions: vec![session],
        recent_messages: Vec::new(),
        unavailable_hosts: Vec::new(),
    }) else {
        panic!("sessions expected");
    };
    assert_eq!(sessions[0].cwd, std::path::PathBuf::new());
    assert_eq!(sessions[0].name, "a");
    let Reply::Projects { projects } = phone_reply(Reply::Projects {
        projects: vec![crate::projects::ProjectView {
            project: "mono".into(),
            dir: "/Users/someone/work/mono".into(),
            master: None,
            master_chosen: false,
            sessions: Vec::new(),
        }],
    }) else {
        panic!("projects expected");
    };
    assert_eq!(projects[0].dir, std::path::PathBuf::new());
}
