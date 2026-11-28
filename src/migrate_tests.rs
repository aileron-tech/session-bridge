use super::*;
use crate::domain::{Provider, SessionId};
use uuid::Uuid;

fn envelope(delivery: Delivery) -> Envelope {
    let id = || SessionId {
        provider: Provider::Claude,
        id: Uuid::new_v4(),
    };
    Envelope {
        message_id: Uuid::new_v4(),
        sender: id(),
        recipient: id(),
        message: "hello".into(),
        project: None,
        delivery,
        created_at: 1,
        scheduled: false,
    }
}

/* REQ-039: Only messages still in flight carry over; finished ones stay behind. */
#[test]
fn carries_only_unfinished_messages() {
    for state in [
        Delivery::Pending { reason: "x".into() },
        Delivery::Dispatching,
        Delivery::Queued,
        Delivery::Forwarded { host: "b".into() },
        Delivery::Held { reason: "x".into() },
    ] {
        assert!(unfinished(&state), "{state:?}");
    }
    for state in [
        Delivery::Delivered,
        Delivery::Refused { reason: "x".into() },
        Delivery::Uncertain { reason: "x".into() },
    ] {
        assert!(!unfinished(&state), "{state:?}");
    }
}

/* REQ-039: The first start copies the config files and folders into the data
folder and starts the history with the unfinished messages; runtime files and
finished messages stay behind, the old folder is left as it was, and a data
folder that exists is never touched. */
#[test]
fn first_start_copies_config_and_unfinished_messages() {
    let home = tempfile::tempdir().unwrap();
    let old = home.path().join("old");
    let data = home.path().join(".session-bridge");
    for dir in ["messages", "roles", "network", "update"] {
        fs::create_dir_all(old.join(dir)).unwrap();
    }
    fs::write(old.join("user.json"), "{\"provider\":\"user\"}").unwrap();
    fs::write(old.join("roles/qa.md"), "You test.").unwrap();
    fs::write(old.join("network/identity.json"), "{}").unwrap();
    fs::write(old.join("daemon.lock"), "").unwrap();
    fs::write(old.join("update/latest.json"), "{}").unwrap();
    let waiting = envelope(Delivery::Pending { reason: "x".into() });
    let done = envelope(Delivery::Delivered);
    for e in [&waiting, &done] {
        store::write_json(
            &old.join("messages").join(format!("{}.json", e.message_id)),
            e,
        )
        .unwrap();
    }

    let outcome = prepare(&old, &data).unwrap();
    assert_eq!(
        outcome,
        Outcome::Copied {
            files: 3,
            unfinished: 1
        }
    );
    assert_eq!(
        fs::read_to_string(data.join("roles/qa.md")).unwrap(),
        "You test."
    );
    assert!(data.join("user.json").is_file());
    assert!(data.join("network/identity.json").is_file());
    for left in ["daemon.lock", "update", "messages"] {
        assert!(!data.join(left).exists(), "{left} was copied");
    }
    let history = store::Store::open(data.clone())
        .unwrap()
        .messages()
        .unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].message_id, waiting.message_id);
    assert!(
        old.join("messages")
            .join(format!("{}.json", done.message_id))
            .is_file()
    );
    assert!(!home.path().join(".session-bridge.migrating").exists());

    fs::write(data.join("user.json"), "changed").unwrap();
    assert_eq!(prepare(&old, &data).unwrap(), Outcome::Present);
    assert_eq!(
        fs::read_to_string(data.join("user.json")).unwrap(),
        "changed"
    );
}

/* REQ-039: Without an old folder, or with one that holds only runtime files
(a new install), the data folder simply starts empty. */
#[test]
fn starts_fresh_without_old_data() {
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join(".session-bridge");
    assert_eq!(
        prepare(&home.path().join("none"), &data).unwrap(),
        Outcome::Fresh
    );
    let runtime = home.path().join("state");
    fs::create_dir_all(&runtime).unwrap();
    fs::write(runtime.join("daemon.lock"), "").unwrap();
    assert_eq!(prepare(&runtime, &data).unwrap(), Outcome::Fresh);
    assert!(!data.exists());
}
