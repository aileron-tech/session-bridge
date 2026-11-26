use super::*;
use proptest::prelude::*;

fn mount(path: &str, total: u64, available: u64) -> Mount {
    Mount {
        path: PathBuf::from(path),
        total,
        available,
    }
}
fn sample(cpu_percent: f32, memory: u64, app_server: bool) -> ProcessSample {
    ProcessSample {
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

/* REQ-029: A session with its own process shows that process; sessions that
share one (a Codex app server, or several sessions in one pid) show the whole
process as shared, never summed or split; a missing process is unavailable. */
#[test]
fn reports_own_shared_and_missing_processes() {
    let sessions = vec![
        ("claude:a".to_owned(), 10),
        ("codex:b".to_owned(), 20),
        ("codex:c".to_owned(), 20),
        ("codex:d".to_owned(), 30),
        ("codex:e".to_owned(), 40),
        ("user:f".to_owned(), 0),
    ];
    let samples = BTreeMap::from([
        (10, sample(3.4, 400 * MB, false)),
        (20, sample(55.0, 2 * GB, true)),
        (30, sample(1.0, 10 * MB, true)),
        (0, sample(9.0, 9, false)),
    ]);
    let views = session_usage(&sessions, &samples);
    let usage: Vec<SessionUsage> = views.iter().map(|v| v.usage).collect();
    assert_eq!(
        usage,
        vec![
            SessionUsage::Own {
                cpu_percent: 3.4,
                memory: 400 * MB
            },
            SessionUsage::Shared {
                cpu_percent: 55.0,
                memory: 2 * GB,
                sessions: 2,
                app_server: true
            },
            SessionUsage::Shared {
                cpu_percent: 55.0,
                memory: 2 * GB,
                sessions: 2,
                app_server: true
            },
            SessionUsage::Shared {
                cpu_percent: 1.0,
                memory: 10 * MB,
                sessions: 1,
                app_server: true
            },
            SessionUsage::Unavailable,
            SessionUsage::Unavailable,
        ]
    );
    assert_eq!(views[0].summary, "CPU 3%, memory 400 MB");
    assert_eq!(
        views[1].summary,
        "shared app server (2 sessions): CPU 55%, memory 2.0 GB"
    );
    assert_eq!(
        views[3].summary,
        "shared app server (1 session): CPU 1%, memory 10.0 MB"
    );
    assert_eq!(views[4].summary, "usage unavailable");

    let two_in_one = session_usage(
        &[("claude:x".into(), 7), ("claude:y".into(), 7)],
        &BTreeMap::from([(7, sample(2.0, 3 * GB, false))]),
    );
    assert_eq!(
        two_in_one[0].summary,
        "shared process (2 sessions): CPU 2%, memory 3.0 GB"
    );
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

/* REQ-029: Host lines read as CPU, memory and disk in the unit of the total;
a missing host reads "usage unavailable". */
#[test]
fn describes_hosts() {
    let usage = HostUsage {
        cpu_percent: 12.4,
        memory_used: 9 * GB + GB / 10,
        memory_total: 16 * GB,
        disk_used: 210 * GB,
        disk_total: 494 * GB,
    };
    let view = host_view("laptop-a".into(), true, HostState::Available { usage });
    assert_eq!(
        view.summary,
        "CPU 12%, memory 9.1 of 16.0 GB, disk 210 of 494 GB"
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
    assert_eq!(json["usage"]["memory_total"], 16 * GB);
    let back: HostUsageView = serde_json::from_value(json).unwrap();
    assert_eq!(back, view);
    assert_eq!(
        describe_host(&HostUsage {
            cpu_percent: f32::NAN,
            memory_used: 512,
            memory_total: 900,
            disk_used: 3 * TB,
            disk_total: 4 * TB,
        }),
        "CPU 0%, memory 512 of 900 B, disk 3.0 of 4.0 TB"
    );
}

proptest! {
    /* REQ-029: Every session gets exactly one view, in order, and no session
    is ever shown a share or a sum of a process: the numbers are the process's. */
    #[test]
    fn never_sums_or_splits(pids in proptest::collection::vec(0u32..5, 0..12), cpu in 0f32..800.0, memory in 0u64..(64 * GB)) {
        let sessions: Vec<(String, u32)> = pids.iter().enumerate().map(|(i, p)| (format!("s{i}"), *p)).collect();
        let samples: BTreeMap<u32, ProcessSample> = (1..4).map(|p| (p, sample(cpu, memory + u64::from(p), p == 3))).collect();
        let views = session_usage(&sessions, &samples);
        prop_assert_eq!(views.len(), sessions.len());
        for (view, (id, pid)) in views.iter().zip(&sessions) {
            prop_assert_eq!(&view.session_id, id);
            let carried = pids.iter().filter(|p| *p == pid).count();
            match view.usage {
                SessionUsage::Own { memory: m, .. } => {
                    prop_assert!(carried == 1 && *pid != 3);
                    prop_assert_eq!(m, memory + u64::from(*pid));
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
