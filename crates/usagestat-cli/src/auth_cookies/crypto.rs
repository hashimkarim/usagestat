use super::*;
use aes::Aes128;
use cbc::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
use pbkdf2::pbkdf2_hmac;
use sha1::Sha1;
use sha2::{Digest, Sha256};
use usagestat_core::process;

#[cfg(windows)]
mod windows;

#[derive(Default)]
pub(super) struct Decoder {
    passwords: Option<Vec<String>>,
    #[cfg(windows)]
    windows_key: Option<Vec<u8>>,
}
pub(super) fn validate_format(platform: Platform, cookie: &Cookie) -> Result<()> {
    if cookie.encrypted.is_empty() {
        return Ok(());
    }
    if !cookie.value.is_empty() {
        return Err(error(
            "COOKIE_DECRYPT_FAILED",
            "Cookie has conflicting plaintext and encrypted values.",
        ));
    }
    if platform == Platform::Windows && cookie.encrypted.starts_with(b"v20") {
        return Err(error(
            "APP_BOUND_UNSUPPORTED",
            "Browser-bound cookies cannot be imported by this backend. Use the provider's supported manual cookie/full-cURL or OAuth/API route; some sessions also remain bound to their browser/device.",
        ));
    }
    if cookie.encrypted.starts_with(b"v10")
        || (platform == Platform::Linux && cookie.encrypted.starts_with(b"v11"))
    {
        return Ok(());
    }
    if platform == Platform::Windows
        && cookie
            .encrypted
            .starts_with(&[1, 0, 0, 0, 0xd0, 0x8c, 0x9d, 0xdf])
    {
        return Ok(());
    }
    Err(error(
        "COOKIE_FORMAT_UNSUPPORTED",
        "Browser encryption format is unsupported. Use provider-specific manual credentials.",
    ))
}
impl Decoder {
    pub(super) fn decode(
        &mut self,
        profile: &Profile,
        cookie: &Cookie,
        version: u32,
    ) -> Result<String> {
        validate_format(profile.platform, cookie)?;
        if cookie.encrypted.is_empty() {
            return Ok(cookie.value.clone());
        }
        let decrypted = match profile.platform {
            Platform::Macos | Platform::Linux => {
                let iterations = if profile.platform == Platform::Macos {
                    1003
                } else {
                    1
                };
                let passwords = if profile.platform == Platform::Linux
                    && cookie.encrypted.starts_with(b"v10")
                {
                    vec!["peanuts".into()]
                } else {
                    if self.passwords.is_none() {
                        self.passwords = Some(passwords(profile)?);
                    }
                    self.passwords.as_ref().unwrap().clone()
                };
                let mut value = None;
                for password in passwords {
                    if let Ok(bytes) = decrypt_cbc(&cookie.encrypted[3..], &password, iterations) {
                        // Schema 24 authenticates the host after CBC decryption;
                        // a coincidental valid padding cannot select the wrong key.
                        if finish_value(&bytes, &cookie.domain, version).is_ok() {
                            value = Some(bytes);
                            break;
                        }
                    }
                }
                value.ok_or_else(|| {
                    error(
                        "COOKIE_DECRYPT_FAILED",
                        "Cookie decryption failed. Sign in again or use manual credentials.",
                    )
                })?
            }
            Platform::Windows => {
                #[cfg(windows)]
                {
                    windows::decrypt(profile, &cookie.encrypted, &mut self.windows_key)?
                }
                #[cfg(not(windows))]
                {
                    return Err(error(
                        "PLATFORM_UNSUPPORTED",
                        "Windows browser decryption requires the original signed-in Windows user.",
                    ));
                }
            }
        };
        finish_value(&decrypted, &cookie.domain, version)
    }
}
pub(super) fn decrypt_cbc(payload: &[u8], password: &str, iterations: u32) -> Result<Vec<u8>> {
    let mut key = [0; 16];
    pbkdf2_hmac::<Sha1>(password.as_bytes(), b"saltysalt", iterations, &mut key);
    cbc::Decryptor::<Aes128>::new(&key.into(), &[b' '; 16].into())
        .decrypt_padded_vec_mut::<Pkcs7>(payload)
        .map_err(|_| error("COOKIE_DECRYPT_FAILED", "Cookie decryption failed."))
}
pub(super) fn finish_value(decrypted: &[u8], domain: &str, version: u32) -> Result<String> {
    let failure = || {
        error(
            "COOKIE_DECRYPT_FAILED",
            "Cookie domain digest or value is invalid; use manual credentials.",
        )
    };
    let value = if version >= 24 {
        let hash = Sha256::digest(domain.as_bytes());
        if decrypted.len() < 32 || decrypted[..32] != hash[..] {
            return Err(failure());
        }
        &decrypted[32..]
    } else {
        decrypted
    };
    let value = std::str::from_utf8(value).map_err(|_| failure())?;
    if !valid_value(value) {
        return Err(failure());
    }
    Ok(value.into())
}
fn passwords(profile: &Profile) -> Result<Vec<String>> {
    match profile.platform {
        Platform::Macos => {
            let (service, account) = profile.mac_keychain.ok_or_else(|| error("COOKIE_FORMAT_UNSUPPORTED", "This browser's macOS Keychain mapping is not yet qualified. Use manual credentials."))?;
            Ok(vec![mac_password(service, account)?])
        }
        Platform::Linux => {
            let mut passwords = Vec::new();
            for id in profile.secret_app_ids {
                let mut command = process::command("secret-tool").map_err(|_| error("KEYCHAIN_UNAVAILABLE", "Install secret-tool and unlock your login keyring, or use manual credentials."))?;
                command.args(["lookup", "application", id]);
                let output =
                    process::run(command, Duration::from_secs(30), 64 * 1024).map_err(|_| {
                        error(
                            "KEYCHAIN_UNAVAILABLE",
                            "Login keyring is unavailable or timed out.",
                        )
                    })?;
                if !output.status.success() {
                    continue;
                }
                let text = String::from_utf8(output.stdout).map_err(|_| {
                    error(
                        "KEYCHAIN_UNAVAILABLE",
                        "Login keyring returned invalid data.",
                    )
                })?;
                let text = text.trim_end_matches(['\r', '\n']);
                if !text.is_empty() {
                    passwords.push(text.into());
                }
            }
            if passwords.is_empty() {
                return Err(error(
                    "KEYCHAIN_UNAVAILABLE",
                    "Browser login-keyring secret was not available. Unlock it or use manual credentials.",
                ));
            }
            Ok(passwords)
        }
        Platform::Windows => unreachable!(),
    }
}
pub(super) fn mac_password(service: &str, account: &str) -> Result<String> {
    mac_password_in(service, account, None)
}

pub(super) fn mac_password_in(
    service: &str,
    account: &str,
    keychain: Option<&std::path::Path>,
) -> Result<String> {
    let mut command = process::command("/usr/bin/security").map_err(|_| {
        error(
            "KEYCHAIN_UNAVAILABLE",
            "macOS Keychain access is unavailable.",
        )
    })?;
    command.args(["find-generic-password", "-s", service, "-a", account, "-w"]);
    if let Some(path) = keychain {
        command.arg(path);
    }
    let output = process::run(command, Duration::from_secs(30), 64 * 1024).map_err(|_| error("KEYCHAIN_UNAVAILABLE", "Keychain request timed out or is unavailable. Unlock your login Keychain or use manual credentials."))?;
    if !output.status.success() {
        return Err(match output.status.code() {
            Some(51 | 128) => error(
                "KEYCHAIN_DENIED",
                "Keychain access was denied or cancelled. Allow the requested item or use manual credentials.",
            ),
            Some(44) => error(
                "KEYCHAIN_UNAVAILABLE",
                "Browser Safe Storage item was not found. Sign in to the browser or use manual credentials.",
            ),
            _ => error(
                "KEYCHAIN_UNAVAILABLE",
                "Browser Keychain item is unavailable or locked. Unlock it or use manual credentials.",
            ),
        });
    }
    let text = String::from_utf8(output.stdout).map_err(|_| {
        error(
            "KEYCHAIN_UNAVAILABLE",
            "Browser Keychain item has invalid data.",
        )
    })?;
    let text = text.trim_end_matches(['\r', '\n']);
    if text.is_empty() {
        return Err(error(
            "KEYCHAIN_UNAVAILABLE",
            "Browser Keychain item is empty.",
        ));
    }
    Ok(text.into())
}
