use super::*;

/* REQ-025: A previous post counts as unread while it waits (pending, held,
dispatching, forwarded), while a Claude post has no receipt, and while a local
Codex post is still in Codex's queue; delivered, refused, uncertain and a
paired laptop's Codex post do not block the next run. */
#[test]
fn decides_unread_from_the_delivery() {
    let reason = || "x".to_owned();
    let (codex, claude) = (Provider::Codex, Provider::Claude);
    for waiting in [
        Delivery::Pending { reason: reason() },
        Delivery::Dispatching,
        Delivery::Held { reason: reason() },
        Delivery::Forwarded { host: "b".into() },
    ] {
        assert_eq!(unread(&waiting, &codex, true), Unread::Yes, "{waiting:?}");
        assert_eq!(unread(&waiting, &claude, false), Unread::Yes, "{waiting:?}");
    }
    assert_eq!(unread(&Delivery::Queued, &claude, true), Unread::Yes);
    assert_eq!(unread(&Delivery::Queued, &claude, false), Unread::Yes);
    assert_eq!(
        unread(&Delivery::Queued, &codex, true),
        Unread::IfInCodexQueue
    );
    assert_eq!(unread(&Delivery::Queued, &codex, false), Unread::No);
    for done in [
        Delivery::Delivered,
        Delivery::Refused { reason: reason() },
        Delivery::Uncertain { reason: reason() },
    ] {
        assert_eq!(unread(&done, &codex, true), Unread::No, "{done:?}");
        assert_eq!(unread(&done, &claude, true), Unread::No, "{done:?}");
    }
}

/* REQ-025: A skipped recipient and the run read plainly, with how long ago
the unread post was sent. */
#[test]
fn reports_runs_and_skips() {
    assert_eq!(ago(5), "just now");
    assert_eq!(ago(20 * 60), "20 min ago");
    assert_eq!(ago(3 * 3600), "3 h ago");
    assert_eq!(ago(3 * 86400), "3 days ago");
    assert_eq!(
        skip_line("master", 1_000, 1_000 + 600),
        "@master: the previous post (10 min ago) was not processed yet; sending skipped"
    );
    let run = ScheduleRun {
        at: 1,
        posted: vec!["dev".into(), "qa".into()],
        skipped: vec![skip_line("master", 0, 1200)],
        failed: vec!["@x: offline".into()],
    };
    assert_eq!(
        summary(&run),
        "posted to @dev, @qa; skipped @master: the previous post (20 min ago) was not processed yet; sending skipped; failed @x: offline"
    );
    assert_eq!(summary(&ScheduleRun::default()), "nobody to post to");
    // Older state files without the newer fields still load.
    let state: ScheduleState = serde_json::from_str("{}").unwrap();
    assert_eq!(state, ScheduleState::default());
}
