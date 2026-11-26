use super::*;
use proptest::prelude::*;

fn asset(name: &str) -> Asset {
    Asset {
        name: name.into(),
        browser_download_url: format!("https://example.com/{name}"),
    }
}
fn release(tag: &str, names: &[&str]) -> Release {
    Release {
        tag_name: tag.into(),
        assets: names.iter().map(|n| asset(n)).collect(),
    }
}

/* REQ-023: Versions and tags compare by major, minor and patch. */
#[test]
fn reads_versions() {
    assert_eq!(version("0.3.0"), Some((0, 3, 0)));
    assert_eq!(version("v1.12.4"), Some((1, 12, 4)));
    assert_eq!(version("v1.2"), None);
    assert_eq!(version("latest"), None);
    assert_eq!(version("1.2.3.4"), None);
    assert!(version("v0.10.0") > version("v0.9.9"));
}

/* REQ-023: Each supported platform has one release asset; others have none. */
#[test]
fn names_platform_assets() {
    assert_eq!(
        asset_name("macos", "aarch64").as_deref(),
        Some("session-bridge-aarch64-apple-darwin")
    );
    assert_eq!(
        asset_name("macos", "x86_64").as_deref(),
        Some("session-bridge-x86_64-apple-darwin")
    );
    assert_eq!(
        asset_name("windows", "x86_64").as_deref(),
        Some("session-bridge-x86_64-pc-windows-msvc.exe")
    );
    assert_eq!(asset_name("linux", "x86_64"), None);
    assert_eq!(asset_name("windows", "aarch64"), None);
}

/* REQ-023: Checksums are read per file name from SHA256SUMS (text or binary mode lines). */
#[test]
fn reads_checksums() {
    let a = "a".repeat(64);
    let b = "0123456789abcdef".repeat(4);
    let sums = format!(
        "{a}  session-bridge-x86_64-apple-darwin\n{b} *session-bridge-aarch64-apple-darwin\nbad line\n"
    );
    assert_eq!(
        checksum_for(&sums, "session-bridge-x86_64-apple-darwin"),
        Some([0xaa; 32])
    );
    let expected: Vec<u8> = (0..32)
        .map(|i| [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef][i % 8])
        .collect();
    assert_eq!(
        checksum_for(&sums, "session-bridge-aarch64-apple-darwin").map(|d| d.to_vec()),
        Some(expected)
    );
    assert_eq!(checksum_for(&sums, "session-bridge"), None);
    assert_eq!(checksum_for(&format!("{}  x", "z".repeat(64)), "x"), None);
}

/* REQ-023: A newer release with this platform's binary and checksums is an
update; the same or an older version is up to date; anything missing is an error. */
#[test]
fn plans_updates() {
    let name = "session-bridge-aarch64-apple-darwin";
    let full = release(
        "v0.4.0",
        &[name, "SHA256SUMS", "session-bridge-x86_64-apple-darwin"],
    );
    assert_eq!(
        plan("0.3.0", &full, "macos", "aarch64"),
        Ok(Plan::Update {
            version: "0.4.0".into(),
            binary: asset(name),
            sums: asset("SHA256SUMS"),
        })
    );
    for current in ["0.4.0", "0.5.0"] {
        assert_eq!(
            plan(current, &full, "macos", "aarch64"),
            Ok(Plan::UpToDate {
                version: current.into()
            })
        );
    }
    assert_eq!(
        plan(
            "0.3.0",
            &release("v0.4.0", &["SHA256SUMS"]),
            "macos",
            "aarch64"
        ),
        Err(UpdateError::NoBinary {
            version: "0.4.0".into(),
            asset: name.into()
        })
    );
    assert_eq!(
        plan("0.3.0", &release("v0.4.0", &[name]), "macos", "aarch64"),
        Err(UpdateError::NoChecksums("0.4.0".into()))
    );
    assert_eq!(
        plan("0.3.0", &release("nightly", &[name]), "macos", "aarch64"),
        Err(UpdateError::BadTag("nightly".into()))
    );
    assert_eq!(
        plan("0.3.0", &full, "linux", "x86_64"),
        Err(UpdateError::UnsupportedPlatform {
            os: "linux".into(),
            arch: "x86_64".into()
        })
    );
}

proptest! {
    /* REQ-023: Version order matches numeric order of the parts. */
    #[test]
    fn version_order(a in (0u64..50, 0u64..50, 0u64..50), b in (0u64..50, 0u64..50, 0u64..50)) {
        let text = |v: (u64, u64, u64)| format!("v{}.{}.{}", v.0, v.1, v.2);
        prop_assert_eq!(version(&text(a)).cmp(&version(&text(b))), a.cmp(&b));
    }
}
