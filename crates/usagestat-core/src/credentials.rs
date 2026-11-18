//! Exact-target generic credentials. Provider mappings belong to providers;
//! never enumerate or guess another application's account/target on writeback.
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CredentialError {
    #[error("credential-missing: no entry at the exact target")]
    Missing,
    #[error("credential-account-mismatch: the exact target belongs to another account")]
    AccountMismatch,
    #[error("credential-denied: access was denied by the native credential store")]
    Denied,
    #[error("credential-unavailable: this logon session has no available credential store")]
    Unavailable,
    #[error("credential-malformed: payload is not valid for the specified encoding")]
    Malformed,
    #[error("credential-invalid: {0}")]
    Invalid(&'static str),
    #[error("credential-unsupported: this operation is not supported on this platform")]
    Unsupported,
    #[error("credential-native-error: Windows status {0}")]
    Native(u32),
}

#[derive(Clone, Copy)]
pub enum Encoding {
    Utf8,
    Utf16Le,
}
impl Encoding {
    pub fn parse(value: &str) -> Result<Self, CredentialError> {
        match value {
            "utf8" => Ok(Self::Utf8),
            "utf16le" => Ok(Self::Utf16Le),
            _ => Err(CredentialError::Invalid("encoding must be utf8 or utf16le")),
        }
    }
    #[cfg(any(windows, test))]
    fn decode(self, bytes: &[u8]) -> Result<String, CredentialError> {
        match self {
            Self::Utf8 => String::from_utf8(bytes.to_vec()).map_err(|_| CredentialError::Malformed),
            Self::Utf16Le if bytes.len() % 2 == 0 => {
                let units: Vec<_> = bytes
                    .chunks_exact(2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]]))
                    .collect();
                String::from_utf16(&units).map_err(|_| CredentialError::Malformed)
            }
            Self::Utf16Le => Err(CredentialError::Malformed),
        }
    }
    #[cfg(any(windows, test))]
    fn encode(self, text: &str) -> Vec<u8> {
        match self {
            Self::Utf8 => text.as_bytes().to_vec(),
            Self::Utf16Le => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        }
    }
}

// Deliberately no Debug derive: callers must not accidentally log passwords.
pub struct PasswordItem {
    pub account: String,
    pub password: String,
}

#[cfg(windows)]
#[path = "credentials_windows.rs"]
mod native;
#[cfg(windows)]
pub use native::{current_user, delete, read, write};

#[cfg(not(windows))]
pub fn read(_: &str, _: Option<&str>, _: Encoding) -> Result<PasswordItem, CredentialError> {
    Err(CredentialError::Unsupported)
}
#[cfg(not(windows))]
pub fn write(_: &str, _: Option<&str>, _: &str, _: Encoding) -> Result<(), CredentialError> {
    Err(CredentialError::Unsupported)
}
#[cfg(not(windows))]
pub fn delete(_: &str, _: Option<&str>) -> Result<(), CredentialError> {
    Err(CredentialError::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_encodings_preserve_non_ascii_json_raw_and_base64_payloads() {
        for text in [
            " raw 使用 café \n",
            r#"{"refresh_token":"synthetic","account":"使用"}"#,
            "go-keyring-base64:ZXhhbXBsZQ==",
            "",
            "embedded\0null",
        ] {
            for encoding in [Encoding::Utf8, Encoding::Utf16Le] {
                assert_eq!(encoding.decode(&encoding.encode(text)).unwrap(), text);
            }
        }
        assert!(matches!(
            Encoding::Utf8.decode(&[0xff]),
            Err(CredentialError::Malformed)
        ));
        assert!(matches!(
            Encoding::Utf16Le.decode(&[1]),
            Err(CredentialError::Malformed)
        ));
        assert!(matches!(
            Encoding::Utf16Le.decode(&[0x00, 0xd8]),
            Err(CredentialError::Malformed)
        ));
        assert!(Encoding::parse("guess").is_err());
    }
}
