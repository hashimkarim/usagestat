use std::fs;
use std::sync::{Arc, Barrier};
use usagestat_core::storage;

#[test]
fn concurrent_creation_publishes_exactly_one_complete_value() {
    let directory = storage::temporary_directory().unwrap();
    let path = directory.path().join("key");
    let barrier = Arc::new(Barrier::new(8));
    let workers: Vec<_> = (0..8)
        .map(|index| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let value = format!("candidate-{index}").repeat(8192);
                barrier.wait();
                let created = storage::create_once(&path, value.as_bytes()).unwrap();
                (created, value)
            })
        })
        .collect();
    let winners: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .filter(|(created, _)| *created)
        .collect();
    assert_eq!(winners.len(), 1);
    assert!(storage::read_private(&path).unwrap() == winners[0].1);
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn replacement_and_append_preserve_complete_content() {
    let directory = storage::temporary_directory().unwrap();
    let path = directory.path().join("使用 state.json");
    storage::write_atomic(&path, b"previous").unwrap();
    storage::write_atomic(&path, b"replacement").unwrap();
    storage::append_private(&path, b"\nrecord\n").unwrap();
    assert_eq!(
        storage::read_private(&path).unwrap(),
        "replacement\nrecord\n"
    );
    assert!(!storage::create_once(&path, b"must-not-replace").unwrap());
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn failed_replacement_leaves_destination_and_cleans_temporary() {
    let directory = storage::temporary_directory().unwrap();
    let path = directory.path().join("directory");
    fs::create_dir(&path).unwrap();
    fs::write(path.join("retained"), b"valid").unwrap();
    assert!(storage::write_atomic(&path, b"replacement").is_err());
    assert_eq!(fs::read(path.join("retained")).unwrap(), b"valid");
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn unix_permissions_are_private_and_symlinks_are_rejected() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let directory = storage::temporary_directory().unwrap();
    let path = directory.path().join("private");
    storage::write_atomic(&path, b"synthetic").unwrap();
    assert_eq!(
        fs::metadata(directory.path()).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    storage::read_private(&path).unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let link = directory.path().join("link");
    symlink(&path, &link).unwrap();
    assert!(storage::write_atomic(&link, b"overwritten").is_err());
    assert!(storage::create_once(&link, b"overwritten").is_err());
    assert!(storage::append_private(&link, b"overwritten").is_err());
    assert_eq!(fs::read(&path).unwrap(), b"synthetic");
}

#[cfg(windows)]
#[test]
fn windows_sharing_violation_is_bounded_and_preserves_last_state() {
    use std::os::windows::fs::OpenOptionsExt;
    use std::time::{Duration, Instant};
    let directory = storage::temporary_directory().unwrap();
    let path = directory.path().join("locked");
    storage::write_atomic(&path, b"previous").unwrap();
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&path)
        .unwrap();
    let start = Instant::now();
    assert!(storage::write_atomic(&path, b"replacement").is_err());
    assert!(start.elapsed() < Duration::from_secs(2));
    drop(held);
    assert_eq!(storage::read_private(&path).unwrap(), "previous");
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}
