//! Private state with same-directory atomic replacement and create-once publication.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

#[cfg(windows)]
#[path = "storage_windows.rs"]
mod platform;

/// Create missing directories privately without changing existing ancestors.
fn create_missing_directories(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty() || path.is_dir() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        create_missing_directories(parent)?;
    }
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    match builder.create(path) {
        Ok(()) => protect_directory(path),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        Err(error) => Err(error),
    }
}

/// Restrict an application-owned directory. Existing ancestors are untouched.
pub fn private_directory(path: &Path) -> io::Result<()> {
    create_missing_directories(path)?;
    reject_link(path)?;
    protect_directory(path)
}

fn parent(path: &Path) -> io::Result<&Path> {
    if path.file_name().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "state path must name a file",
        ));
    }
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .or(Some(Path::new(".")))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "state file needs a parent directory",
            )
        })
}

fn reject_link(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            #[cfg(windows)]
            let link = {
                use std::os::windows::fs::MetadataExt;
                metadata.file_attributes() & 0x400 != 0
            };
            #[cfg(not(windows))]
            let link = metadata.file_type().is_symlink();
            if link {
                Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "private state path must not be a symlink or reparse point",
                ))
            } else {
                Ok(())
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn protect(file: &File, directory: bool) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    if file.metadata()?.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private state must be owned by the current user",
        ));
    }
    file.set_permissions(fs::Permissions::from_mode(if directory {
        0o700
    } else {
        0o600
    }))
}

#[cfg(windows)]
fn protect(file: &File, directory: bool) -> io::Result<()> {
    platform::protect(file, directory)
}

fn protect_directory(path: &Path) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.read(true);
    no_follow(&mut options);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::{FILE_READ_ATTRIBUTES, READ_CONTROL, WRITE_DAC};
        // Open directories with the ACL rights up front. ReOpenFile is useful
        // for tempfile's file handles, but fails on native directory handles.
        options.access_mode(FILE_READ_ATTRIBUTES.0 | READ_CONTROL.0 | WRITE_DAC.0);
        options.custom_flags(0x0200_0000 | 0x0020_0000); // BACKUP_SEMANTICS | OPEN_REPARSE_POINT
    }
    let file = options.open(path).map_err(|error| {
        io::Error::new(error.kind(), format!("open private directory: {error}"))
    })?;
    let metadata = file.metadata()?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private state directory is not a directory",
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "private state directory is a reparse point",
            ));
        }
    }
    protect(&file, true)
}

fn no_follow(options: &mut OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
}

fn validate_file(file: &File) -> io::Result<()> {
    let metadata = file.metadata()?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "private state is a reparse point",
            ));
        }
    }
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private state must be a regular file",
        ));
    }
    protect(file, false)
}

pub fn read_private(path: &Path) -> io::Result<String> {
    reject_link(path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    no_follow(&mut options);
    let mut file = options.open(path)?;
    validate_file(&file)?;
    let mut value = String::new();
    file.read_to_string(&mut value)?;
    Ok(value)
}

fn temporary_file(path: &Path) -> io::Result<tempfile::NamedTempFile> {
    create_missing_directories(parent(path)?)?;
    reject_link(path)?;
    let file = tempfile::Builder::new()
        .prefix(".usagestat-state-")
        .tempfile_in(parent(path)?)?;
    protect(file.as_file(), false)?;
    Ok(file)
}

/// The previous file survives failed writes or replacement. Bytes are private
/// before the first write; no remove-then-rename fallback is used on Windows.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_atomic_with(path, |file| file.write_all(bytes))
}

fn write_atomic_with(
    path: &Path,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    let mut temporary = temporary_file(path)?;
    write(temporary.as_file_mut())?;
    temporary.as_file().sync_all()?;
    let deadline = Instant::now() + Duration::from_millis(500);
    loop {
        match temporary.persist(path) {
            Ok(_) => return Ok(()),
            Err(error) => {
                // Windows readers/antivirus may briefly deny delete sharing.
                let retry =
                    cfg!(windows) && matches!(error.error.raw_os_error(), Some(5 | 32 | 33));
                if !retry || Instant::now() >= deadline {
                    return Err(error.error);
                }
                temporary = error.file;
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

/// Publish a complete file only if absent. A competing writer's file wins intact.
pub fn create_once(path: &Path, bytes: &[u8]) -> io::Result<bool> {
    let mut temporary = temporary_file(path)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    match temporary.persist_noclobber(path) {
        Ok(_) => Ok(true),
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error.error),
    }
}

pub fn append_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    open_private_append(path)?.write_all(bytes)
}

pub fn open_private_append(path: &Path) -> io::Result<File> {
    create_missing_directories(parent(path)?)?;
    reject_link(path)?;
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    no_follow(&mut options);
    let file = options.open(path)?;
    validate_file(&file)?;
    Ok(file)
}

#[cfg(windows)]
pub fn current_user_sid() -> io::Result<String> {
    platform::current_sid()
}

#[cfg(windows)]
pub fn account_sid(account: &str) -> io::Result<String> {
    platform::account_sid(account)
}

pub fn temporary_directory() -> io::Result<tempfile::TempDir> {
    let directory = tempfile::Builder::new()
        .prefix("usagestat-private-")
        .tempdir()?;
    private_directory(directory.path())?;
    Ok(directory)
}

/// Hold a kernel lock for an application-owned process identity. Never unlink
/// this file while running: another process could otherwise lock a new inode.
pub fn exclusive_lock(path: &Path) -> io::Result<File> {
    create_missing_directories(parent(path)?)?;
    reject_link(path)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    no_follow(&mut options);
    let file = options.open(path)?;
    validate_file(&file)?;
    file.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => io::Error::new(
            io::ErrorKind::WouldBlock,
            "private state is already locked by another process",
        ),
        std::fs::TryLockError::Error(error) => error,
    })?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_writer_child() {
        let Some(directory) = std::env::var_os("USAGESTAT_TEST_INTERRUPTED_WRITE") else {
            return;
        };
        let directory = std::path::PathBuf::from(directory);
        write_atomic_with(&directory.join("state.json"), |file| {
            file.write_all(b"incomplete replacement")?;
            file.sync_all()?;
            fs::write(directory.join("ready"), b"ready")?;
            loop {
                std::thread::park();
            }
        })
        .unwrap();
    }

    #[test]
    fn process_termination_preserves_last_committed_state() {
        let directory = temporary_directory().unwrap();
        let path = directory.path().join("state.json");
        write_atomic(&path, b"committed state").unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "storage::tests::interrupted_writer_child"])
            .env("USAGESTAT_TEST_INTERRUPTED_WRITE", directory.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !directory.path().join("ready").exists() && Instant::now() < deadline {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let ready = directory.path().join("ready").exists();
        let _ = child.kill();
        child.wait().unwrap();
        assert!(ready, "writer did not reach the unpublished write");
        assert!(read_private(&path).unwrap() == "committed state");
        // An uncatchable process kill cannot run destructors. Any abandoned
        // temporary is private and is never mistaken for committed state.
        for entry in fs::read_dir(directory.path()).unwrap() {
            let entry = entry.unwrap();
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with(".usagestat-state-")
            {
                assert!(read_private(&entry.path()).is_ok());
            }
        }
        write_atomic(&path, b"recovered state").unwrap();
        assert!(read_private(&path).unwrap() == "recovered state");
    }

    #[test]
    fn failed_write_retains_last_state_and_removes_temporary() {
        let directory = temporary_directory().unwrap();
        let path = directory.path().join("state.json");
        write_atomic(&path, b"valid").unwrap();
        let error = write_atomic_with(&path, |file| {
            file.write_all(b"partial")?;
            Err(io::Error::other("injected interruption"))
        });
        assert!(error.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"valid");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
