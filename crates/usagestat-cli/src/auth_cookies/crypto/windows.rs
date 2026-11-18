use super::*;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::io::Read;

#[repr(C)]
struct Blob {
    len: u32,
    data: *mut u8,
}
#[link(name = "crypt32")]
unsafe extern "system" {
    fn CryptUnprotectData(
        input: *const Blob,
        description: *mut *mut u16,
        entropy: *const Blob,
        reserved: *mut std::ffi::c_void,
        prompt: *mut std::ffi::c_void,
        flags: u32,
        output: *mut Blob,
    ) -> i32;
    #[cfg(test)]
    fn CryptProtectData(
        input: *const Blob,
        description: *const u16,
        entropy: *const Blob,
        reserved: *mut std::ffi::c_void,
        prompt: *mut std::ffi::c_void,
        flags: u32,
        output: *mut Blob,
    ) -> i32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn LocalFree(memory: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
}
struct OwnedBlob(Blob);
impl Drop for OwnedBlob {
    fn drop(&mut self) {
        if !self.0.data.is_null() {
            // Free native allocation on every path; never retain a borrowed DPAPI buffer.
            unsafe {
                std::ptr::write_bytes(self.0.data, 0, self.0.len as usize);
                LocalFree(self.0.data.cast());
            }
        }
    }
}
fn dpapi(data: &[u8], protect: bool) -> Result<Vec<u8>> {
    if data.is_empty() || data.len() > 2 * 1024 * 1024 {
        return Err(error(
            "COOKIE_DECRYPT_FAILED",
            "Windows encrypted data is invalid.",
        ));
    }
    let input = Blob {
        len: data.len() as u32,
        data: data.as_ptr().cast_mut(),
    };
    let mut output = OwnedBlob(Blob {
        len: 0,
        data: std::ptr::null_mut(),
    });
    let ok = unsafe {
        if protect {
            #[cfg(test)]
            {
                CryptProtectData(
                    &input,
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    1,
                    &mut output.0,
                )
            }
            #[cfg(not(test))]
            {
                return Err(error(
                    "COOKIE_DECRYPT_FAILED",
                    "Browser credential writes are unavailable.",
                ));
            }
        } else {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                1,
                &mut output.0,
            )
        }
    };
    if ok == 0 || output.0.data.is_null() {
        return Err(error(
            "KEYCHAIN_UNAVAILABLE",
            "Windows cannot decrypt this browser credential for the signed-in user. Use the original user profile or manual credentials.",
        ));
    }
    Ok(unsafe { std::slice::from_raw_parts(output.0.data, output.0.len as usize) }.to_vec())
}
fn load_key(profile: &Profile) -> Result<Vec<u8>> {
    let failure = || {
        error(
            "KEYCHAIN_UNAVAILABLE",
            "The selected browser Local State key is unavailable. Use manual credentials.",
        )
    };
    let file = std::fs::File::open(profile.data_dir.join("Local State")).map_err(|_| failure())?;
    let mut data = Vec::new();
    file.take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut data)
        .map_err(|_| failure())?;
    if data.len() > 2 * 1024 * 1024 {
        return Err(failure());
    }
    let state: serde_json::Value = serde_json::from_slice(&data).map_err(|_| failure())?;
    let encoded = state
        .pointer("/os_crypt/encrypted_key")
        .and_then(|v| v.as_str())
        .ok_or_else(failure)?;
    let key = STANDARD.decode(encoded).map_err(|_| failure())?;
    let Some(blob) = key.strip_prefix(b"DPAPI") else {
        return Err(error(
            "COOKIE_FORMAT_UNSUPPORTED",
            "Browser key format is unsupported; use manual credentials.",
        ));
    };
    let key = dpapi(blob, false)?;
    if key.len() != 32 {
        return Err(failure());
    }
    Ok(key)
}
pub(super) fn decrypt(
    profile: &Profile,
    encrypted: &[u8],
    cached_key: &mut Option<Vec<u8>>,
) -> Result<Vec<u8>> {
    if encrypted.starts_with(b"v10") {
        if encrypted.len() < 3 + 12 + 16 {
            return Err(error(
                "COOKIE_DECRYPT_FAILED",
                "Windows cookie ciphertext is truncated.",
            ));
        }
        if cached_key.is_none() {
            *cached_key = Some(load_key(profile)?);
        }
        let cipher = Aes256Gcm::new_from_slice(cached_key.as_ref().unwrap())
            .map_err(|_| error("COOKIE_DECRYPT_FAILED", "Windows cookie key is invalid."))?;
        cipher
            .decrypt(Nonce::from_slice(&encrypted[3..15]), &encrypted[15..])
            .map_err(|_| {
                error(
                    "COOKIE_DECRYPT_FAILED",
                    "Windows cookie authentication failed. Use manual credentials.",
                )
            })
    } else {
        dpapi(encrypted, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disposable_same_user_dpapi_and_aes_cookie_formats() {
        let root = tempfile::tempdir().unwrap();
        let profile = Profile {
            browser: "chrome",
            name: "Default".into(),
            data_dir: root.path().into(),
            db: root.path().join("Cookies"),
            platform: Platform::Windows,
            secret_app_ids: &[],
            mac_keychain: None,
        };
        let key = [42u8; 32];
        let wrapped = dpapi(&key, true).unwrap();
        let mut pref = b"DPAPI".to_vec();
        pref.extend(wrapped);
        std::fs::write(
            root.path().join("Local State"),
            serde_json::to_vec(
                &serde_json::json!({"os_crypt":{"encrypted_key":STANDARD.encode(pref)}}),
            )
            .unwrap(),
        )
        .unwrap();
        let mut plain = Sha256::digest(b".example.test").to_vec();
        plain.extend(b"synthetic-cookie");
        let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
        let nonce = [24u8; 12];
        let mut encrypted = b"v10".to_vec();
        encrypted.extend(nonce);
        encrypted.extend(
            cipher
                .encrypt(Nonce::from_slice(&nonce), plain.as_slice())
                .unwrap(),
        );
        let mut cache = None;
        assert_eq!(decrypt(&profile, &encrypted, &mut cache).unwrap(), plain);
        assert_eq!(
            finish_value(&plain, ".example.test", 24).unwrap(),
            "synthetic-cookie"
        );
        *encrypted.last_mut().unwrap() ^= 1;
        assert!(decrypt(&profile, &encrypted, &mut cache).is_err());
        let legacy = dpapi(b"synthetic-legacy-cookie", true).unwrap();
        assert_eq!(
            decrypt(&profile, &legacy, &mut cache).unwrap(),
            b"synthetic-legacy-cookie"
        );
        assert!(dpapi(b"malformed", false).is_err());
        assert!(!root.path().join("Cookies").exists());
    }
}
