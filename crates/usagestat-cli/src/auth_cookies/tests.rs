use super::*;
use rusqlite::params;
use std::fs;
use std::path::Path;

fn fixture(root: &Path, name: &str, platform: Platform) -> Profile {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    let db = dir.join("Cookies");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch("CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT); INSERT INTO meta VALUES('version','24'); CREATE TABLE cookies(host_key TEXT,name TEXT,value TEXT,encrypted_value BLOB,path TEXT,expires_utc INTEGER,is_secure INTEGER,top_frame_site_key TEXT);").unwrap();
    Profile {
        browser: "chrome",
        name: name.into(),
        db,
        data_dir: root.into(),
        platform,
        secret_app_ids: &[],
        mac_keychain: Some(("unused-fixture-service", "unused-fixture-account")),
    }
}
fn insert(
    profile: &Profile,
    domain: &str,
    name: &str,
    value: &str,
    path: &str,
    expiry: i64,
    secure: bool,
    partition: &str,
) {
    Connection::open(&profile.db)
        .unwrap()
        .execute(
            "INSERT INTO cookies VALUES(?1,?2,?3,x'',?4,?5,?6,?7)",
            params![domain, name, value, path, expiry, secure, partition],
        )
        .unwrap();
}
fn url() -> reqwest::Url {
    reqwest::Url::parse("https://chat.example.test/api/usage").unwrap()
}

#[test]
fn scoped_snapshot_filters_domains_paths_expiry_transport_and_partitioned_cookies() {
    let root = tempfile::tempdir().unwrap();
    let p = fixture(root.path(), "Default 使用", Platform::Linux);
    insert(&p, ".example.test", "parent", "short", "/", 0, true, "");
    insert(
        &p,
        "chat.example.test",
        "session",
        "exact",
        "/api",
        0,
        true,
        "",
    );
    insert(&p, ".example.test", "session", "other", "/", 0, true, "");
    for (domain, name, path, expiry, partition) in [
        ("example.test", "host_only_parent", "/", 0, ""),
        ("notchat.example.test", "wrong_host", "/", 0, ""),
        ("evilchat.example.test", "lookalike", "/", 0, ""),
        (".chat.example.test.evil", "wrong_suffix", "/", 0, ""),
        ("chat.example.test", "wrong_path", "/api/u", 0, ""),
        ("chat.example.test", "expired", "/", 1, ""),
        (
            "chat.example.test",
            "partitioned",
            "/",
            0,
            "https://other.test",
        ),
    ] {
        insert(
            &p,
            domain,
            name,
            "must-not-import",
            path,
            expiry,
            true,
            partition,
        );
    }
    let snap = read_snapshot(&p, &url(), chromium_now()).unwrap();
    assert_eq!(
        build_header(snap.cookies, "chat.example.test"),
        "session=exact; parent=short"
    );
    assert!(
        read_snapshot(
            &p,
            &reqwest::Url::parse("http://chat.example.test/api/usage").unwrap(),
            chromium_now()
        )
        .unwrap()
        .cookies
        .is_empty()
    );
    assert!(!path_matches("/api", "/apix"));
    assert!(path_matches("/api/", "/api/usage"));
}

#[test]
fn live_wal_snapshot_reads_only_committed_changes_without_copies_or_writes() {
    let root = tempfile::tempdir().unwrap();
    let p = fixture(root.path(), "Default", Platform::Linux);
    insert(&p, ".example.test", "session", "before", "/", 0, true, "");
    let writer = Connection::open(&p.db).unwrap();
    writer
        .execute_batch("PRAGMA journal_mode=WAL; BEGIN IMMEDIATE; UPDATE cookies SET value='after'")
        .unwrap();
    assert_eq!(
        read_snapshot(&p, &url(), chromium_now()).unwrap().cookies[0].value,
        "before"
    );
    writer.execute_batch("COMMIT").unwrap();
    assert_eq!(
        read_snapshot(&p, &url(), chromium_now()).unwrap().cookies[0].value,
        "after"
    );
    writer
        .execute_batch("PRAGMA journal_mode=DELETE; BEGIN EXCLUSIVE")
        .unwrap();
    let start = std::time::Instant::now();
    assert!(read_snapshot(&p, &url(), chromium_now()).is_err());
    assert!(start.elapsed() < Duration::from_secs(3));
    writer.execute_batch("ROLLBACK").unwrap();
    let mut missing = p.clone();
    missing.db = root.path().join("missing");
    assert!(read_snapshot(&missing, &url(), chromium_now()).is_err());
    assert!(!missing.db.exists());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
}

#[test]
fn profile_ambiguity_and_known_unsupported_formats_do_not_request_any_key() {
    let root = tempfile::tempdir().unwrap();
    let first = fixture(root.path(), "Default", Platform::Windows);
    let second = fixture(root.path(), "Profile 1", Platform::Windows);
    for p in [&first, &second] {
        insert(p, ".example.test", "session", "synthetic", "/", 0, true, "");
    }
    let mut calls = 0;
    let result = import_selected("fixture", &url(), vec![first.clone(), second], |_, _, _| {
        calls += 1;
        Ok("other".into())
    });
    assert_eq!(result.err().unwrap().error, "AMBIGUOUS_PROFILE");
    assert_eq!(calls, 0);
    Connection::open(&first.db)
        .unwrap()
        .execute(
            "UPDATE cookies SET value='',encrypted_value=?1",
            [b"v20synthetic-bound-data".as_slice()],
        )
        .unwrap();
    assert_eq!(
        import_selected("fixture", &url(), vec![first.clone()], |_, _, _| {
            calls += 1;
            Ok("other".into())
        })
        .err()
        .unwrap()
        .error,
        "APP_BOUND_UNSUPPORTED"
    );
    assert_eq!(calls, 0);
    Connection::open(&first.db)
        .unwrap()
        .execute(
            "UPDATE cookies SET encrypted_value=?1",
            [b"v10synthetic-encrypted-data".as_slice()],
        )
        .unwrap();
    assert_eq!(
        import_selected("fixture", &url(), vec![first], |_, _, _| Err(error(
            "KEYCHAIN_DENIED",
            "fixture denial"
        )))
        .err()
        .unwrap()
        .error,
        "KEYCHAIN_DENIED"
    );
}

#[test]
fn schema_digest_and_cookie_bytes_are_exact_and_never_heuristically_repaired() {
    use sha2::{Digest, Sha256};
    let mut value = Sha256::digest(b".example.test").to_vec();
    value.extend(b"synthetic%20session+token=");
    assert_eq!(
        crypto::finish_value(&value, ".example.test", 24).unwrap(),
        "synthetic%20session+token="
    );
    assert!(crypto::finish_value(&value, "other.test", 24).is_err());
    assert_eq!(
        crypto::finish_value(
            b"01234567890123456789012345678901eyJtoken",
            ".example.test",
            23
        )
        .unwrap(),
        "01234567890123456789012345678901eyJtoken"
    );
    for bad in [
        b"session\r\ninjection".as_slice(),
        b"token; other=secret",
        b"\xffmalformed",
    ] {
        assert!(crypto::finish_value(bad, ".example.test", 23).is_err());
    }
    let root = tempfile::tempdir().unwrap();
    let p = fixture(root.path(), "Default", Platform::Linux);
    Connection::open(&p.db)
        .unwrap()
        .execute("UPDATE meta SET value='25'", [])
        .unwrap();
    assert_eq!(
        read_snapshot(&p, &url(), chromium_now())
            .err()
            .unwrap()
            .error,
        "COOKIE_SCHEMA_UNSUPPORTED"
    );
}

#[test]
fn native_browser_roots_profile_names_and_overrides_are_authoritative() {
    for platform in [Platform::Linux, Platform::Macos, Platform::Windows] {
        let root = tempfile::tempdir().unwrap();
        let p = fixture(root.path(), "Profile 使用 with spaces", platform);
        let opts = ImportOptions {
            browser: Some("chrome".into()),
            profile: Some(p.name.clone()),
            user_data_dir: Some(root.path().into()),
        };
        let profiles = profiles::discover_with(platform, &opts, |_| {
            panic!("Explicit root fell through to another installation")
        })
        .unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].db, p.db);
        let bad = ImportOptions {
            profile: Some("../other-account".into()),
            ..opts
        };
        assert!(profiles::discover_with(platform, &bad, |_| None).is_err());
    }
    let opts = ImportOptions {
        browser: Some("firefox".into()),
        ..Default::default()
    };
    assert_eq!(
        profiles::discover_with(Platform::Linux, &opts, |_| None)
            .err()
            .unwrap()
            .error,
        "BROWSER_UNSUPPORTED"
    );
}

#[test]
fn cbc_platform_derivation_matches_independent_openssl_vectors() {
    // Generated with Python hashlib PBKDF2 + OpenSSL enc AES-128-CBC,
    // independent of the Rust CBC implementation. Includes schema-24 host hash.
    for (iterations, hex) in [
        (
            1,
            "231732984a63007013418932afed07589b5e5241af0abb1670d8b87f56d624ada7ae7406d5d9f4c61bdc90e77e7fe7b38729def7c5283b9d38c6378cf8d897be",
        ),
        (
            1003,
            "1ed2c90284df3c92295d12077eddf3a39d9f5e797292a851f735783b36e7d42c4abbedf464da04ca8e637e594c0bc317b13d615a494771a8baee5e81bf16d0e2",
        ),
    ] {
        let bytes = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect::<Vec<_>>();
        let plain = crypto::decrypt_cbc(&bytes, "fixture-password", iterations).unwrap();
        assert_eq!(
            crypto::finish_value(&plain, ".example.test", 24).unwrap(),
            "synthetic-session+token%3D"
        );
        assert!(crypto::decrypt_cbc(&bytes, "wrong-password", iterations).is_err());
    }
}

#[cfg(target_os = "macos")]
#[test]
fn native_disposable_browser_keychain_item_is_exact_and_read_only() {
    use usagestat_core::process;
    fn security(args: &[&std::ffi::OsStr]) -> bool {
        let mut command = process::command("/usr/bin/security").unwrap();
        command.args(args);
        process::run(command, Duration::from_secs(30), 4096)
            .unwrap()
            .status
            .success()
    }
    let root = tempfile::tempdir().unwrap();
    let keychain = root.path().join("synthetic-browser.keychain-db");
    let password = std::ffi::OsStr::new("synthetic-test-keychain-password");
    // Hosted runners may have a locked login keychain. Test our own store
    // without unlocking or replacing the user's login keychain.
    assert!(security(&[
        "create-keychain".as_ref(),
        "-p".as_ref(),
        password,
        keychain.as_os_str()
    ]));
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let mut command = process::command("/usr/bin/security").unwrap();
            command.arg("delete-keychain").arg(&self.0);
            let _ = process::run(command, Duration::from_secs(30), 4096);
        }
    }
    let cleanup = Cleanup(keychain.clone());
    assert!(security(&[
        "unlock-keychain".as_ref(),
        "-p".as_ref(),
        password,
        keychain.as_os_str()
    ]));
    let service = "usagestat-synthetic-browser-service";
    let account = "synthetic-browser-account";
    assert!(crypto::mac_password_in(service, account, Some(&keychain)).is_err());
    assert!(security(&[
        "add-generic-password".as_ref(),
        "-s".as_ref(),
        service.as_ref(),
        "-a".as_ref(),
        account.as_ref(),
        "-w".as_ref(),
        " synthetic browser key ".as_ref(),
        keychain.as_os_str()
    ]));
    assert_eq!(
        crypto::mac_password_in(service, account, Some(&keychain)).unwrap(),
        " synthetic browser key "
    );
    assert!(crypto::mac_password_in(service, "different-account", Some(&keychain)).is_err());
    assert_eq!(
        crypto::mac_password_in(service, account, Some(&keychain)).unwrap(),
        " synthetic browser key "
    );
    drop(cleanup);
    assert!(!keychain.exists());
}
