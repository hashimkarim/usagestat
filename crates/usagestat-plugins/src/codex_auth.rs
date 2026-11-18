//! Codex-owned authentication formats, selected within one native profile.
//! No enumeration, migration, key generation or encrypted-store writeback.
use std::io::Read;
use std::path::Path;

use age::secrecy::SecretString;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
#[cfg(windows)]
use usagestat_core::credentials;
use usagestat_core::provider_paths;

const LIMIT: u64 = 2 * 1024 * 1024;
const MISSING: &str =
    "credential-missing: Codex auth is absent in the selected profile; run codex login";
const MALFORMED: &str =
    "credential-malformed: Codex auth data is invalid or uses an unsupported schema";
const UNAVAILABLE: &str = "credential-unavailable: Codex auth storage is inaccessible";
const CONFLICT: &str =
    "credential-account-mismatch: Codex auth changed; retry after the CLI finishes signing in";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Store {
    File,
    Direct,
    Encrypted,
    AutoDirect,
    AutoEncrypted,
}

// Deliberately no Debug: this object contains the selected application's tokens.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AuthState {
    auth: Value,
    source: &'static str,
    storage: &'static str,
    profile_key: String,
    revision: String,
    read_only: bool,
}

fn hash(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn profile_hash(root: &Path) -> String {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_owned());
    hash(&canonical.to_string_lossy())[..16].into()
}

fn read_file(path: &Path) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|error| {
        match error.kind() {
            std::io::ErrorKind::NotFound => MISSING,
            std::io::ErrorKind::PermissionDenied => {
                "credential-denied: Codex auth file access was denied"
            }
            _ => UNAVAILABLE,
        }
        .to_owned()
    })?;
    if !file.metadata().map_err(|_| UNAVAILABLE)?.is_file() {
        return Err(MALFORMED.into());
    }
    let mut bytes = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| UNAVAILABLE)?;
    if bytes.len() as u64 > LIMIT {
        return Err(MALFORMED.into());
    }
    Ok(bytes)
}

fn select_store(root: &Path, explicit: Option<&str>, windows: bool) -> Result<Store, String> {
    if let Some(explicit) = explicit.filter(|value| !value.is_empty() && *value != "auto") {
        return match explicit {
            "file" => Ok(Store::File),
            "keyring" => Ok(Store::Direct),
            "encrypted" => Ok(Store::Encrypted),
            _ => {
                Err("failed: settings.authStorage must be auto, file, keyring or encrypted".into())
            }
        };
    }
    let config = match read_file(&root.join("config.toml")) {
        Ok(bytes) => std::str::from_utf8(&bytes)
            .ok()
            .and_then(|text| text.parse::<toml::Value>().ok())
            .ok_or("failed: Codex config.toml is invalid; check the selected profile")?,
        Err(error) if error == MISSING => toml::Value::Table(Default::default()),
        Err(error) => return Err(error),
    };
    let encrypted = match config
        .get("features")
        .and_then(|features| features.get("secret_auth_storage"))
    {
        Some(value) => value
            .as_bool()
            .ok_or("failed: Codex secret_auth_storage must be a boolean")?,
        None => windows,
    };
    let mode = match config.get("cli_auth_credentials_store") {
        Some(value) => value
            .as_str()
            .ok_or("failed: Codex auth store setting must be a string")?,
        None => "file",
    };
    match mode {
        "file" => Ok(Store::File),
        "keyring" => Ok(if encrypted { Store::Encrypted } else { Store::Direct }),
        "auto" => Ok(if encrypted { Store::AutoEncrypted } else { Store::AutoDirect }),
        "ephemeral" => Err("unsupported: Codex in-memory authentication cannot be read by another process; select a persistent Codex auth store".into()),
        _ => Err("unsupported: Codex authentication storage mode is not supported".into()),
    }
}

fn read_keyring(service: &str, account: &str) -> Result<String, String> {
    #[cfg(windows)]
    return credentials::read(
        &format!("{account}.{service}"),
        Some(account),
        credentials::Encoding::Utf16Le,
    )
    .map(|item| item.password)
    .map_err(|error| error.to_string());
    #[cfg(not(windows))]
    crate::host_api::platform_keychain_read(service, Some(account)).map_err(|error| {
        if error.starts_with("credential-") {
            error
        } else {
            UNAVAILABLE.into()
        }
    })
}

fn decrypt_auth(bytes: &[u8], passphrase: String) -> Result<String, String> {
    let decryptor = age::Decryptor::new(bytes).map_err(|_| MALFORMED)?;
    // Exactly one scrypt recipient, preventing repeated expensive key derivations.
    if !decryptor.is_scrypt() {
        return Err(MALFORMED.into());
    }
    let mut identity = age::scrypt::Identity::new(SecretString::from(passphrase));
    identity.set_max_work_factor(20);
    let reader = decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .map_err(|error| {
            if matches!(error, age::DecryptError::ExcessiveWork { .. }) {
                "unsupported: Codex encrypted auth exceeds the supported key-derivation work limit"
            } else {
                MALFORMED
            }
        })?;
    let mut plain = Vec::new();
    reader
        .take(LIMIT + 1)
        .read_to_end(&mut plain)
        .map_err(|_| MALFORMED)?;
    if plain.len() as u64 > LIMIT {
        return Err(MALFORMED.into());
    }
    let payload: Value = serde_json::from_slice(&plain).map_err(|_| MALFORMED)?;
    if !matches!(payload.get("version").and_then(Value::as_u64), Some(0 | 1)) {
        return Err(MALFORMED.into());
    }
    payload
        .get("secrets")
        .and_then(|secrets| secrets.get("global/CODEX_AUTH"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(MISSING.into())
}

fn read_at(
    root: &Path,
    store: Store,
    keyring: impl Fn(&str, &str) -> Result<String, String>,
) -> Result<AuthState, String> {
    let profile_key = profile_hash(root);
    let file = || {
        read_file(&root.join("auth.json"))
            .and_then(|bytes| String::from_utf8(bytes).map_err(|_| MALFORMED.into()))
    };
    let direct = || keyring("Codex Auth", &format!("cli|{profile_key}"));
    let encrypted = || {
        let bytes = read_file(&root.join("secrets/codex_auth.age"))?;
        let passphrase = keyring("codex", &format!("secrets|{profile_key}"))?;
        decrypt_auth(&bytes, passphrase)
    };
    let (result, mut storage) = match store {
        Store::File => (file(), "file"),
        Store::Direct | Store::AutoDirect => (direct(), "keyring"),
        Store::Encrypted | Store::AutoEncrypted => (encrypted(), "encrypted"),
    };
    let text = match result {
        Ok(text) => text,
        Err(error)
            if matches!(store, Store::AutoDirect | Store::AutoEncrypted)
                && error.starts_with("credential-missing:") =>
        {
            storage = "file";
            file()?
        }
        Err(error) => return Err(error),
    };
    let auth: Value = serde_json::from_str(&text).map_err(|_| MALFORMED)?;
    if !auth.is_object() {
        return Err(MALFORMED.into());
    }
    // Hash normalized JSON so formatting differences alone do not look like a race.
    let revision = hash(&serde_json::to_string(&auth).map_err(|_| MALFORMED)?);
    Ok(AuthState {
        auth,
        source: "native",
        storage,
        profile_key,
        revision,
        read_only: storage == "encrypted",
    })
}

pub(crate) fn read(explicit: Option<&str>) -> Result<AuthState, String> {
    let root = provider_paths::codex_home()
        .map_err(|_| "failed: Codex profile directory unavailable; check CODEX_HOME")?;
    read_at(
        &root,
        select_store(&root, explicit, cfg!(windows))?,
        read_keyring,
    )
}

pub(crate) fn write(
    explicit: Option<&str>,
    profile_key: &str,
    revision: &str,
    storage: &str,
    text: &str,
) -> Result<(), String> {
    if text.len() as u64 > LIMIT {
        return Err(MALFORMED.into());
    }
    let state = read(explicit)?;
    if state.read_only {
        return Err(
            "unsupported: Refresh encrypted Codex credentials through the Codex CLI".into(),
        );
    }
    if state.profile_key != profile_key || state.revision != revision || state.storage != storage {
        return Err(CONFLICT.into());
    }
    let auth: Value = serde_json::from_str(text).map_err(|_| MALFORMED)?;
    if !auth.is_object()
        || state.auth.pointer("/tokens/account_id") != auth.pointer("/tokens/account_id")
    {
        return Err(CONFLICT.into());
    }
    let root = provider_paths::codex_home().map_err(|_| CONFLICT)?;
    if profile_hash(&root) != state.profile_key {
        return Err(CONFLICT.into());
    }
    if storage == "file" {
        return usagestat_core::storage::write_atomic(&root.join("auth.json"), text.as_bytes())
            .map_err(|_| UNAVAILABLE.into());
    }
    let account = format!("cli|{profile_key}");
    #[cfg(windows)]
    return credentials::write(
        &format!("{account}.Codex Auth"),
        Some(&account),
        text,
        credentials::Encoding::Utf16Le,
    )
    .map_err(|error| error.to_string());
    #[cfg(not(windows))]
    crate::host_api::platform_keychain_write("Codex Auth", Some(&account), text)
        .map_err(|_| UNAVAILABLE.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn fixture_auth() -> Value {
        serde_json::json!({"tokens": {"access_token": "synthetic-access", "refresh_token": "synthetic-refresh", "account_id": "fixture-account"}})
    }

    fn encrypt_fixture(payload: &Value, passphrase: &str) -> Vec<u8> {
        let mut recipient = age::scrypt::Recipient::new(SecretString::from(passphrase.to_owned()));
        recipient.set_work_factor(1); // Fast synthetic fixtures, never production encryption.
        age::encrypt(&recipient, &serde_json::to_vec(payload).unwrap()).unwrap()
    }

    #[test]
    fn file_auth_has_no_keyring_side_effects_and_keeps_profile_selection() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        std::fs::write(
            root.join("auth.json"),
            serde_json::to_vec(&fixture_auth()).unwrap(),
        )
        .unwrap();
        let state = read_at(root, Store::File, |_, _| {
            panic!("file auth consulted a credential store")
        })
        .unwrap();
        assert_eq!(state.auth, fixture_auth());
        assert!(!state.read_only);
        assert_eq!(state.storage, "file");
        let other = tempfile::tempdir().unwrap();
        assert_ne!(state.profile_key, profile_hash(other.path()));
        assert!(matches!(
            select_store(root, None, true).unwrap(),
            Store::File
        ));
        std::fs::write(
            root.join("config.toml"),
            "cli_auth_credentials_store = 'keyring'\n",
        )
        .unwrap();
        assert!(matches!(
            select_store(root, None, true).unwrap(),
            Store::Encrypted
        ));
        assert!(matches!(
            select_store(root, None, false).unwrap(),
            Store::Direct
        ));
        assert!(matches!(
            select_store(root, Some("keyring"), true).unwrap(),
            Store::Direct
        ));
        std::fs::write(
            root.join("config.toml"),
            "cli_auth_credentials_store = 'ephemeral'\n",
        )
        .unwrap();
        assert!(
            select_store(root, None, false)
                .err()
                .unwrap()
                .starts_with("unsupported:")
        );
    }

    #[test]
    fn auto_store_falls_back_only_for_missing_and_never_denied_or_malformed() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::write(
            fixture.path().join("auth.json"),
            serde_json::to_vec(&fixture_auth()).unwrap(),
        )
        .unwrap();
        let missing = read_at(
            fixture.path(),
            Store::AutoDirect,
            |_, _| Err(MISSING.into()),
        )
        .unwrap();
        assert_eq!(missing.storage, "file");
        for message in [
            "credential-denied: fixture",
            "credential-unavailable: fixture",
            MALFORMED,
        ] {
            let denied = read_at(
                fixture.path(),
                Store::AutoDirect,
                |_, _| Err(message.into()),
            );
            assert_eq!(denied.err().unwrap(), message);
        }
        let malformed = read_at(fixture.path(), Store::AutoDirect, |_, _| {
            Ok("invalid-json".into())
        });
        assert_eq!(malformed.err().unwrap(), MALFORMED);
    }

    #[test]
    fn encrypted_codex_auth_selects_exact_master_key_and_auth_namespace() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let payload = serde_json::json!({"version": 1, "secrets": {
            "global/CODEX_AUTH": fixture_auth().to_string(), "global/UNRELATED": "not-auth"
        }});
        let bytes = encrypt_fixture(&payload, "synthetic-passphrase");
        std::fs::create_dir(root.join("secrets")).unwrap();
        let path = root.join("secrets/codex_auth.age");
        std::fs::write(&path, &bytes).unwrap();
        let calls = Cell::new(0);
        let state = read_at(root, Store::Encrypted, |service, account| {
            calls.set(calls.get() + 1);
            assert_eq!(service, "codex");
            assert_eq!(account, format!("secrets|{}", profile_hash(root)));
            Ok("synthetic-passphrase".into())
        })
        .unwrap();
        assert_eq!(calls.get(), 1);
        assert_eq!(state.auth, fixture_auth());
        assert!(state.read_only);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(
            decrypt_auth(&bytes, "wrong-passphrase".into())
                .err()
                .unwrap(),
            MALFORMED
        );
        let mut tampered = bytes.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert_eq!(
            decrypt_auth(&tampered, "synthetic-passphrase".into())
                .err()
                .unwrap(),
            MALFORMED
        );
        let unsupported = encrypt_fixture(
            &serde_json::json!({"version": 99, "secrets": {}}),
            "synthetic-passphrase",
        );
        assert_eq!(
            decrypt_auth(&unsupported, "synthetic-passphrase".into())
                .err()
                .unwrap(),
            MALFORMED
        );
        // No master key is generated when an encrypted file exists without one.
        assert_eq!(
            read_at(root, Store::Encrypted, |_, _| Err(MISSING.into()))
                .err()
                .unwrap(),
            MISSING
        );
    }

    #[cfg(windows)]
    #[test]
    fn native_windows_codex_direct_and_encrypted_credentials_are_exact_and_utf16() {
        use credentials::{CredentialError, Encoding};
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("Codex 使用 account");
        std::fs::create_dir(&root).unwrap();
        let hash = profile_hash(&root);
        let direct_account = format!("cli|{hash}");
        let master_account = format!("secrets|{hash}");
        let direct_target = format!("{direct_account}.Codex Auth");
        let master_target = format!("{master_account}.codex");
        struct Cleanup(Vec<(String, String)>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                for (target, account) in &self.0 {
                    let _ = credentials::delete(target, Some(account));
                }
            }
        }
        let mut cleanup = Cleanup(vec![]);
        for (target, account, value) in [
            (&direct_target, &direct_account, fixture_auth().to_string()),
            (
                &master_target,
                &master_account,
                "synthetic-passphrase".into(),
            ),
        ] {
            assert!(matches!(
                credentials::read(target, None, Encoding::Utf16Le),
                Err(CredentialError::Missing)
            ));
            credentials::write(target, Some(account), &value, Encoding::Utf16Le).unwrap();
            cleanup.0.push((target.clone(), account.clone()));
        }
        assert_eq!(
            read_at(&root, Store::Direct, read_keyring).unwrap().auth,
            fixture_auth()
        );
        std::fs::create_dir(root.join("secrets")).unwrap();
        let payload = serde_json::json!({"version": 1, "secrets": {"global/CODEX_AUTH": fixture_auth().to_string()}});
        std::fs::write(
            root.join("secrets/codex_auth.age"),
            encrypt_fixture(&payload, "synthetic-passphrase"),
        )
        .unwrap();
        assert_eq!(
            read_at(&root, Store::Encrypted, read_keyring).unwrap().auth,
            fixture_auth()
        );
        assert!(matches!(
            credentials::read(&direct_target, Some("another-account"), Encoding::Utf16Le),
            Err(CredentialError::AccountMismatch)
        ));
    }
}
