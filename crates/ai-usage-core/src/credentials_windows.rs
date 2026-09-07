use super::*;
use windows::Win32::Foundation::*;
use windows::Win32::Security::Credentials::*;
use windows::Win32::System::WindowsProgramming::GetUserNameW;
use windows::core::{PCWSTR, PWSTR};

type Result<T> = std::result::Result<T, CredentialError>;
struct Owned(*mut CREDENTIALW);
impl Drop for Owned {
    fn drop(&mut self) {
        unsafe {
            CredFree(self.0.cast());
        }
    }
}
fn error(error: windows::core::Error) -> CredentialError {
    classify(error.code().0 as u32 & 0xffff)
}
fn classify(code: u32) -> CredentialError {
    match code {
        code if code == ERROR_NOT_FOUND.0 => CredentialError::Missing,
        code if code == ERROR_ACCESS_DENIED.0 => CredentialError::Denied,
        code if code == ERROR_NO_SUCH_LOGON_SESSION.0 || code == ERROR_NOT_LOGGED_ON.0 => {
            CredentialError::Unavailable
        }
        code => CredentialError::Native(code),
    }
}
fn wide(value: &str, max: u32) -> Result<Vec<u16>> {
    let text: Vec<_> = value.encode_utf16().collect();
    if text.is_empty() || text.contains(&0) || text.len() > max as usize {
        return Err(CredentialError::Invalid(
            "target/account must be nonempty, NUL-free, and within native limits",
        ));
    }
    Ok(text.into_iter().chain([0]).collect())
}
fn load(target: &str, account: Option<&str>) -> Result<Owned> {
    let target = wide(target, CRED_MAX_GENERIC_TARGET_NAME_LENGTH)?;
    if let Some(account) = account {
        wide(account, CRED_MAX_USERNAME_LENGTH)?;
    }
    let mut value = std::ptr::null_mut();
    unsafe { CredReadW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None, &mut value) }
        .map_err(error)?;
    if value.is_null() {
        return Err(CredentialError::Malformed);
    }
    let value = Owned(value);
    let actual = username(&value)?;
    if account.is_some_and(|account| account != actual) {
        return Err(CredentialError::AccountMismatch);
    }
    Ok(value)
}
fn username(value: &Owned) -> Result<String> {
    let username = unsafe { (*value.0).UserName };
    if username.is_null() {
        return Ok(String::new());
    }
    unsafe { username.to_string() }.map_err(|_| CredentialError::Malformed)
}
pub fn current_user() -> Result<String> {
    let mut buffer = vec![0u16; 257];
    let mut size = buffer.len() as u32;
    unsafe { GetUserNameW(Some(PWSTR(buffer.as_mut_ptr())), &mut size) }.map_err(error)?;
    let text = buffer
        .get(..size.saturating_sub(1) as usize)
        .ok_or(CredentialError::Malformed)?;
    String::from_utf16(text).map_err(|_| CredentialError::Malformed)
}
pub fn read(target: &str, account: Option<&str>, encoding: Encoding) -> Result<PasswordItem> {
    let value = load(target, account)?;
    let credential = unsafe { &*value.0 };
    let size = credential.CredentialBlobSize as usize;
    if size > CRED_MAX_CREDENTIAL_BLOB_SIZE as usize
        || (size > 0 && credential.CredentialBlob.is_null())
    {
        return Err(CredentialError::Malformed);
    }
    let bytes = if size == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(credential.CredentialBlob, size) }
    };
    Ok(PasswordItem {
        account: username(&value)?,
        password: encoding.decode(bytes)?,
    })
}
// Serialize this process's read-check-write operations. Windows does not expose
// an atomic compare-and-swap against another application's concurrent refresh.
static MUTATIONS: std::sync::Mutex<()> = std::sync::Mutex::new(());
pub fn write(
    target: &str,
    account: Option<&str>,
    password: &str,
    encoding: Encoding,
) -> Result<()> {
    let mut bytes = encoding.encode(password);
    if bytes.len() > CRED_MAX_CREDENTIAL_BLOB_SIZE as usize {
        return Err(CredentialError::Invalid(
            "encoded credential exceeds the native blob limit",
        ));
    }
    let _lock = MUTATIONS.lock().unwrap_or_else(|e| e.into_inner());
    let existing = match load(target, account) {
        Ok(value) => Some(value),
        Err(CredentialError::Missing) => None,
        Err(error) => return Err(error),
    };
    let mut target = wide(target, CRED_MAX_GENERIC_TARGET_NAME_LENGTH)?;
    let mut user;
    // Preserve the exact existing target, username, persistence, comment,
    // attributes and alias during refresh. Only the secret blob is replaced.
    let mut value = if let Some(existing) = &existing {
        unsafe { *existing.0 }
    } else {
        let account = account
            .map(str::to_owned)
            .map(Ok)
            .unwrap_or_else(current_user)?;
        user = wide(&account, CRED_MAX_USERNAME_LENGTH)?;
        CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: PWSTR(target.as_mut_ptr()),
            UserName: PWSTR(user.as_mut_ptr()),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            ..Default::default()
        }
    };
    value.CredentialBlob = bytes.as_mut_ptr();
    value.CredentialBlobSize = bytes.len() as u32;
    unsafe { CredWriteW(&value, 0) }.map_err(error)
}
pub fn delete(target: &str, account: Option<&str>) -> Result<()> {
    let _lock = MUTATIONS.lock().unwrap_or_else(|e| e.into_inner());
    let value = load(target, account)?;
    unsafe { CredDeleteW((*value.0).TargetName, CRED_TYPE_GENERIC, None) }.map_err(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Disposable(String);
    impl Drop for Disposable {
        fn drop(&mut self) {
            let _ = delete(&self.0, None);
        }
    }
    #[test]
    fn native_disposable_roundtrip_checks_accounts_and_preserves_refresh_metadata() {
        let target = format!(
            "usagestat-native-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        assert!(matches!(
            read(&target, None, Encoding::Utf8),
            Err(CredentialError::Missing)
        ));
        let _cleanup = Disposable(target.clone());
        assert!(!current_user().unwrap().is_empty());
        for encoding in [Encoding::Utf8, Encoding::Utf16Le] {
            write(
                &target,
                Some("使用-account"),
                "synthetic credential café",
                encoding,
            )
            .unwrap();
            assert_eq!(
                read(&target, Some("使用-account"), encoding)
                    .unwrap()
                    .password,
                "synthetic credential café"
            );
            assert!(matches!(
                read(&target, Some("other-account"), encoding),
                Err(CredentialError::AccountMismatch)
            ));
            assert!(matches!(
                write(&target, Some("other-account"), "unrelated", encoding),
                Err(CredentialError::AccountMismatch)
            ));
            assert!(matches!(
                delete(&target, Some("other-account")),
                Err(CredentialError::AccountMismatch)
            ));
            {
                let original = load(&target, None).unwrap();
                let mut original_value = unsafe { *original.0 };
                let mut comment: Vec<u16> = "synthetic retained comment"
                    .encode_utf16()
                    .chain([0])
                    .collect();
                let mut alias: Vec<u16> = "synthetic-alias".encode_utf16().chain([0]).collect();
                original_value.Comment = PWSTR(comment.as_mut_ptr());
                original_value.TargetAlias = PWSTR(alias.as_mut_ptr());
                original_value.Persist = CRED_PERSIST_SESSION;
                unsafe { CredWriteW(&original_value, 0) }.unwrap();
            }
            write(
                &target,
                None,
                r#"{"refresh_token":"synthetic-updated"}"#,
                encoding,
            )
            .unwrap();
            let refreshed = load(&target, None).unwrap();
            let raw = unsafe { &*refreshed.0 };
            assert_eq!(raw.Persist, CRED_PERSIST_SESSION);
            assert_eq!(
                unsafe { raw.Comment.to_string() }.unwrap(),
                "synthetic retained comment"
            );
            assert_eq!(
                unsafe { raw.TargetAlias.to_string() }.unwrap(),
                "synthetic-alias"
            );
            assert_eq!(
                read(&target, None, encoding).unwrap().account,
                "使用-account"
            );
            let old = read(&target, None, encoding).unwrap().password;
            assert!(write(&target, None, &"x".repeat(6000), encoding).is_err());
            assert_eq!(read(&target, None, encoding).unwrap().password, old);
            delete(&target, Some("使用-account")).unwrap();
            assert!(matches!(
                read(&target, None, encoding),
                Err(CredentialError::Missing)
            ));
        }
    }
    #[test]
    fn concurrent_native_current_user_roundtrips_remain_visible() {
        // Distinct targets must not disappear when another provider writes its
        // own credential concurrently. No retries and no real provider targets.
        let account = current_user().unwrap();
        std::thread::scope(|scope| {
            for worker in 0..4 {
                let account = &account;
                scope.spawn(move || {
                    for iteration in 0..16 {
                        let target = format!("usagestat-concurrent-test-{}-{worker}-{iteration}-{}",
                            std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
                        assert!(matches!(read(&target, None, Encoding::Utf8), Err(CredentialError::Missing)));
                        let _cleanup = Disposable(target.clone());
                        write(&target, Some(account), "synthetic concurrent credential", Encoding::Utf8).unwrap();
                        match read(&target, Some(account), Encoding::Utf8) {
                            Ok(item) => assert_eq!(item.password, "synthetic concurrent credential"),
                            Err(error) => {
                                let direct = match read(&target, None, Encoding::Utf8) {
                                    Ok(_) => "entry-present".to_owned(),
                                    Err(error) => error.to_string(),
                                };
                                panic!("current-user read after write failed for worker {worker}, iteration {iteration}: {error}; diagnostic recheck: {direct}");
                            }
                        }
                    }
                });
            }
        });
    }

    #[test]
    fn native_failure_categories_are_distinct_and_do_not_include_secrets() {
        assert_eq!(classify(ERROR_NOT_FOUND.0), CredentialError::Missing);
        assert_eq!(classify(ERROR_ACCESS_DENIED.0), CredentialError::Denied);
        assert_eq!(
            classify(ERROR_NO_SUCH_LOGON_SESSION.0),
            CredentialError::Unavailable
        );
        assert_eq!(classify(12345), CredentialError::Native(12345));
    }
}
