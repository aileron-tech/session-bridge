use super::*;
use proptest::prelude::*;

fn mount(path: &str, total: u64, available: u64) -> Mount {
    Mount {
        path: PathBuf::from(path),
        total,
        available,
    }
}
fn row(pid: u32, parent: u32, cpu_percent: f32, memory: u64, app_server: bool) -> ProcessRow {
    ProcessRow {
        pid,
        parent: Some(parent),
        cpu_percent,
        memory,
        app_server,
    }
}

/* REQ-029: The disk shown is the mount that holds the home: the longest
containing mount path wins, and a home outside every mount has no disk. */
#[test]
fn picks_the_mount_that_holds_the_home() {
    let mounts = [
        mount("/", 500, 100),
        mount("/Users", 1000, 250),
        mount("/Users/other", 10, 5),
        mount("/Volumes/backup", 2000, 2000),
    ];
    assert_eq!(
        home_disk(Path::new("/Users/me"), &mounts),
        Some((750, 1000))
    );
    assert_eq!(home_disk(Path::new("/opt/me"), &mounts), Some((400, 500)));
    assert_eq!(home_disk(Path::new("/Users/me"), &mounts[2..]), None);
    // A mount whose name only shares a prefix is not a parent.
    assert_eq!(
        home_disk(Path::new("/Users/otherwise"), &mounts[2..3]),
        None
    );
    // Available above total (a misreport) never underflows.
    assert_eq!(
        home_disk(Path::new("/x"), &[mount("/", 5, 9)]),
        Some((0, 5))
    );
}

/* REQ-029: A session with its own process shows its process tree (a build
it runs counts) up to another session started from it, which is its own row;
sessions that share one process (a Codex app server, or
several sessions in one pid) show that process alone, marked shared, never
summed or split; a missing process is unavailable. */
#[test]
fn reports_own_trees_shared_and_missing_processes() {
    let sessions = vec![
        ("claude:a".to_owned(), 10),
        ("codex:b".to_owned(), 20),
        ("codex:c".to_owned(), 20),
        ("codex:d".to_owned(), 30),
        ("codex:e".to_owned(), 40),
        ("user:f".to_owned(), 0),
        ("claude:g".to_owned(), 50),
    ];
    let table = [
        row(10, 1, 3.0, 400 * MIB, false),
        row(11, 10, 250.0, GIB, false), // cargo build under claude:a
        row(12, 11, 50.0, 100 * MIB, false),
        row(50, 11, 4.0, 200 * MIB, false), // claude:g, started from claude:a's shell
        row(51, 50, 6.0, 300 * MIB, false),
        row(20, 1, 55.0, 2 * GIB, true),
        row(21, 20, 90.0, GIB, false), // a child of the app server: not counted
        row(30, 1, 1.0, 10 * MIB, true),
        row(0, 0, 9.0, 9, false),
    ];
    let views = session_usage(&sessions, &table);
    let usage: Vec<SessionUsage> = views.iter().map(|v| v.usage).collect();
    let shared = SessionUsage::Shared {
        cpu_percent: 55.0,
        memory: 2 * GIB,
        sessions: 2,
        app_server: true,
    };
    assert_eq!(
        usage,
        vec![
            SessionUsage::Own {
                cpu_percent: 303.0,
                memory: GIB + 500 * MIB,
                processes: 3
            },
            shared,
            shared,
            SessionUsage::Shared {
                cpu_percent: 1.0,
                memory: 10 * MIB,
                sessions: 1,
                app_server: true
            },
            SessionUsage::Unavailable,
            SessionUsage::Unavailable,
            SessionUsage::Own {
                cpu_percent: 10.0,
                memory: 500 * MIB,
                processes: 2
            },
        ]
    );
    assert_eq!(views[0].summary, "CPU 303%, memory 1.5 GiB, 3 processes");
    assert_eq!(
        views[1].summary,
        "shared app server (2 sessions, main process only): CPU 55%, memory 2.0 GiB"
    );
    assert_eq!(
        views[3].summary,
        "shared app server (1 session, main process only): CPU 1%, memory 10.0 MiB"
    );
    assert_eq!(views[4].summary, "usage unavailable");

    let two_in_one = session_usage(
        &[("claude:x".into(), 7), ("claude:y".into(), 7)],
        &[row(7, 1, 2.0, 3 * GIB, false), row(8, 7, 50.0, GIB, false)],
    );
    assert_eq!(
        two_in_one[0].summary,
        "shared process (2 sessions, main process only): CPU 2%, memory 3.0 GiB"
    );
    // A parent loop in a misread table counts each process once.
    let looped = session_usage(
        &[("claude:z".into(), 5)],
        &[row(5, 6, 1.0, MIB, false), row(6, 5, 1.0, MIB, false)],
    );
    assert_eq!(looped[0].summary, "CPU 2%, memory 2.0 MiB, 2 processes");
}

/* REQ-029: A Codex app server is recognized by its subcommand only. */
#[test]
fn recognizes_the_codex_app_server() {
    let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert!(is_app_server(&args(&[
        "codex",
        "app-server",
        "--listen",
        "x"
    ])));
    assert!(is_app_server(&args(&["/opt/codex", "--x", "app-server"])));
    assert!(!is_app_server(&args(&["app-server"])));
    assert!(!is_app_server(&args(&["codex", "--remote", "unix://s"])));
    assert!(!is_app_server(&args(&["vim", "app-server.md"])));
}

/* REQ-029: Host lines read as CPU, memory and disk in binary units named as
such, in the unit of the total; a missing host reads "usage unavailable". */
#[test]
fn describes_hosts() {
    let usage = HostUsage {
        cpu_percent: 12.4,
        memory_used: 9 * GIB + GIB / 10,
        memory_total: 16 * GIB,
        disk_used: 210 * GIB,
        disk_total: 494 * GIB,
    };
    let view = host_view("laptop-a".into(), true, HostState::Available { usage });
    assert_eq!(
        view.summary,
        "CPU 12%, memory 9.1 of 16.0 GiB, disk 210 of 494 GiB"
    );
    let gone = host_view(
        "laptop-b".into(),
        false,
        HostState::Unavailable {
            reason: "timed out".into(),
        },
    );
    assert_eq!(gone.summary, "usage unavailable");
    let json = serde_json::to_value(&gone).unwrap();
    assert_eq!(json["state"], "unavailable");
    assert_eq!(json["reason"], "timed out");
    let json = serde_json::to_value(&view).unwrap();
    assert_eq!(json["state"], "available");
    assert_eq!(json["usage"]["memory_total"], 16 * GIB);
    let back: HostUsageView = serde_json::from_value(json).unwrap();
    assert_eq!(back, view);
    assert_eq!(
        describe_host(&HostUsage {
            cpu_percent: f32::NAN,
            memory_used: 512,
            memory_total: 900,
            disk_used: 3 * TIB,
            disk_total: 4 * TIB,
        }),
        "CPU 0%, memory 512 of 900 B, disk 3.0 of 4.0 TiB"
    );
}

/* REQ-029: A paired bridge that answers is asked again after 30 s; one that
fails is asked after 30 s, then 60, 120, 240 and at most every 300 s; an
answer resets the backoff. */
#[test]
fn backs_off_from_failing_bridges() {
    let secs = Duration::from_secs;
    let down = || HostState::Unavailable {
        reason: "offline".into(),
    };
    let up = HostState::Available {
        usage: HostUsage {
            cpu_percent: 1.0,
            memory_used: 1,
            memory_total: 2,
            disk_used: 1,
            disk_total: 2,
        },
    };
    assert!(remote_due(None, secs(0)));
    let ok = next_remote(None, up.clone());
    assert_eq!(ok.failures, 0);
    assert!(!remote_due(Some(&ok), secs(29)));
    assert!(remote_due(Some(&ok), secs(30)));
    let mut sample = next_remote(Some(&ok), down());
    for (failures, wait) in [
        (1, 30),
        (2, 60),
        (3, 120),
        (4, 240),
        (5, 300),
        (6, 300),
        (40, 300),
    ] {
        while sample.failures < failures {
            sample = next_remote(Some(&sample), down());
        }
        assert!(!remote_due(Some(&sample), secs(wait - 1)), "{failures}");
        assert!(remote_due(Some(&sample), secs(wait)), "{failures}");
    }
    assert_eq!(next_remote(Some(&sample), up).failures, 0);
}

/* REQ-029: A query that has started keeps a second poll from asking the same
bridge, until it is older than any query may take. */
#[test]
fn holds_a_query_in_flight() {
    assert!(!query_in_flight(None));
    assert!(query_in_flight(Some(Duration::ZERO)));
    assert!(query_in_flight(Some(
        REMOTE_QUERY_HOLD - Duration::from_millis(1)
    )));
    assert!(!query_in_flight(Some(REMOTE_QUERY_HOLD)));
}

proptest! {
    /* REQ-029: Every session gets exactly one view, in order; a shared
    process is reported alone (never its children, never summed or split),
    and an own process with its tree. */
    #[test]
    fn never_sums_or_splits_shared(pids in proptest::collection::vec(0u32..5, 0..12), cpu in 0f32..800.0, memory in 0u64..(64 * GIB)) {
        let sessions: Vec<(String, u32)> = pids.iter().enumerate().map(|(i, p)| (format!("s{i}"), *p)).collect();
        // Pids 1..4 exist, 3 is an app server; each has one child 10+pid.
        let table: Vec<ProcessRow> = (1..4)
            .flat_map(|p| [row(p, 0, cpu, memory + u64::from(p), p == 3), row(10 + p, p, 1.0, 7, false)])
            .collect();
        let views = session_usage(&sessions, &table);
        prop_assert_eq!(views.len(), sessions.len());
        for (view, (id, pid)) in views.iter().zip(&sessions) {
            prop_assert_eq!(&view.session_id, id);
            let carried = pids.iter().filter(|p| *p == pid).count();
            match view.usage {
                SessionUsage::Own { memory: m, processes, .. } => {
                    prop_assert!(carried == 1 && *pid != 3);
                    prop_assert_eq!(processes, 2);
                    prop_assert_eq!(m, memory + u64::from(*pid) + 7);
                }
                SessionUsage::Shared { memory: m, sessions, .. } => {
                    prop_assert_eq!(sessions, carried);
                    prop_assert_eq!(m, memory + u64::from(*pid));
                }
                SessionUsage::Unavailable => prop_assert!(*pid == 0 || *pid == 4),
            }
        }
    }
}
