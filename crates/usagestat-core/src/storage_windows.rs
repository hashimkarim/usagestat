use std::fs::File;
use std::io;
use std::os::windows::io::AsRawHandle;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_INSUFFICIENT_BUFFER, HANDLE, HLOCAL, LocalFree,
};
use windows::Win32::Security::{self, Authorization::*, *};
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{BOOL, PCWSTR, PWSTR};

struct OwnedHandle(HANDLE);
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
struct LocalMemory(*mut core::ffi::c_void);
impl Drop for LocalMemory {
    fn drop(&mut self) {
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.0)));
        }
    }
}

fn win_error(error: windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error(error.code().0 & 0xffff)
}

fn context(operation: &str, error: windows::core::Error) -> io::Error {
    let error = win_error(error);
    io::Error::new(error.kind(), format!("{operation}: {error}"))
}

fn sid_string(sid: PSID) -> io::Result<String> {
    let mut value = PWSTR::null();
    unsafe { ConvertSidToStringSidW(sid, &mut value) }.map_err(win_error)?;
    let _memory = LocalMemory(value.0.cast());
    unsafe { value.to_string() }.map_err(|_| io::Error::other("invalid SID text"))
}

pub(super) fn current_sid() -> io::Result<String> {
    token_sid(TokenUser)
}

pub(super) fn account_sid(account: &str) -> io::Result<String> {
    if account.is_empty() || account.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "account name must be nonempty and NUL-free",
        ));
    }
    let name: Vec<u16> = account.encode_utf16().chain([0]).collect();
    let mut sid_size = 0;
    let mut domain_size = 0;
    let mut kind = SID_NAME_USE::default();
    let result = unsafe {
        LookupAccountNameW(
            PCWSTR::null(),
            PCWSTR(name.as_ptr()),
            None,
            &mut sid_size,
            None,
            &mut domain_size,
            &mut kind,
        )
    };
    if let Err(error) = result {
        if error.code() != windows::core::HRESULT::from_win32(ERROR_INSUFFICIENT_BUFFER.0) {
            return Err(win_error(error));
        }
    }
    if sid_size == 0 || sid_size > 65536 || domain_size > 32768 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid native account lookup size",
        ));
    }
    let mut sid = vec![0usize; (sid_size as usize).div_ceil(size_of::<usize>())];
    let mut domain = vec![0u16; domain_size as usize];
    unsafe {
        LookupAccountNameW(
            PCWSTR::null(),
            PCWSTR(name.as_ptr()),
            Some(PSID(sid.as_mut_ptr().cast())),
            &mut sid_size,
            Some(PWSTR(domain.as_mut_ptr())),
            &mut domain_size,
            &mut kind,
        )
    }
    .map_err(win_error)?;
    sid_string(PSID(sid.as_mut_ptr().cast()))
}

fn token_sid(class: TOKEN_INFORMATION_CLASS) -> io::Result<String> {
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.map_err(win_error)?;
    let token = OwnedHandle(token);
    let mut length = 0;
    unsafe {
        let _ = GetTokenInformation(token.0, class, None, 0, &mut length);
    }
    if length == 0 {
        return Err(io::Error::last_os_error());
    }
    // TOKEN_USER requires pointer alignment; a byte Vec does not guarantee it.
    let mut buffer = vec![0usize; (length as usize).div_ceil(size_of::<usize>())];
    unsafe {
        GetTokenInformation(
            token.0,
            class,
            Some(buffer.as_mut_ptr().cast()),
            length,
            &mut length,
        )
    }
    .map_err(win_error)?;
    if class == TokenUser {
        let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
        sid_string(user.User.Sid)
    } else {
        let owner = unsafe { &*buffer.as_ptr().cast::<TOKEN_OWNER>() };
        sid_string(owner.Owner)
    }
}

pub(super) fn protect(file: &File, directory: bool) -> io::Result<()> {
    let sid = current_sid()?;
    // Reopen the same file object with WRITE_DAC; never reopen a temporary by
    // pathname between its creation and the first secret write.
    let reopened = if directory {
        None // protect_directory already opened with READ_CONTROL | WRITE_DAC.
    } else {
        Some(OwnedHandle(
            unsafe {
                ReOpenFile(
                    HANDLE(file.as_raw_handle()),
                    READ_CONTROL.0 | WRITE_DAC.0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    FILE_FLAGS_AND_ATTRIBUTES(0),
                )
            }
            .map_err(|error| context("reopen private object for ACL update", error))?,
        ))
    };
    let handle = reopened
        .as_ref()
        .map_or(HANDLE(file.as_raw_handle()), |h| h.0);
    let mut owner = PSID::default();
    let mut owner_descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            None,
            None,
            Some(&mut owner_descriptor),
        )
    }
    .ok()
    .map_err(|error| context("read private object owner", error))?;
    let _owner_memory = LocalMemory(owner_descriptor.0);
    let owner_sid = sid_string(owner)?;
    if owner_sid != sid && owner_sid != token_sid(TokenOwner)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private state must be owned by the current user",
        ));
    }
    let inherit = if directory { "OICI" } else { "" };
    let descriptor: Vec<u16> = format!("D:P(A;{inherit};FA;;;{sid})(A;{inherit};FA;;;SY)")
        .encode_utf16()
        .chain([0])
        .collect();
    let mut security = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(descriptor.as_ptr()),
            1,
            &mut security,
            None,
        )
    }
    .map_err(win_error)?;
    let _memory = LocalMemory(security.0);
    let mut present = BOOL::default();
    let mut defaulted = BOOL::default();
    let mut acl = std::ptr::null_mut();
    unsafe {
        Security::GetSecurityDescriptorDacl(security, &mut present, &mut acl, &mut defaulted)
    }
    .map_err(win_error)?;
    if !present.as_bool() || acl.is_null() {
        return Err(io::Error::other("private ACL is missing"));
    }
    unsafe {
        SetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(acl),
            None,
        )
    }
    .ok()
    .map_err(|error| context("apply private object ACL", error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::ffi::OsStrExt;

    #[test]
    fn native_acl_allows_only_current_user_and_system_with_inheritance_disabled() {
        let directory = crate::storage::temporary_directory().unwrap();
        let path = directory.path().join("credential");
        crate::storage::write_atomic(&path, b"synthetic").unwrap();
        for (path, directory) in [(directory.path(), true), (path.as_path(), false)] {
            let name: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
            let mut acl = std::ptr::null_mut();
            let mut descriptor = PSECURITY_DESCRIPTOR::default();
            unsafe {
                GetNamedSecurityInfoW(
                    PCWSTR(name.as_ptr()),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    None,
                    None,
                    Some(&mut acl),
                    None,
                    &mut descriptor,
                )
            }
            .ok()
            .unwrap();
            let _memory = LocalMemory(descriptor.0);
            let mut control = 0;
            let mut revision = 0;
            unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) }
                .unwrap();
            assert_ne!(control & SE_DACL_PROTECTED.0, 0);
            assert!(!acl.is_null());
            assert_eq!(unsafe { (*acl).AceCount }, 2);
            let mut trustees = Vec::new();
            for index in 0..2 {
                let mut raw = std::ptr::null_mut();
                unsafe { GetAce(acl, index, &mut raw) }.unwrap();
                let ace = unsafe { &*raw.cast::<ACCESS_ALLOWED_ACE>() };
                assert_eq!(ace.Header.AceType, 0); // ACCESS_ALLOWED_ACE_TYPE
                assert_eq!(ace.Header.AceFlags, if directory { 3 } else { 0 });
                assert_eq!(ace.Mask, FILE_ALL_ACCESS.0);
                trustees.push(
                    sid_string(PSID(std::ptr::addr_of!(ace.SidStart).cast_mut().cast())).unwrap(),
                );
            }
            trustees.sort();
            let mut expected = vec![current_sid().unwrap(), "S-1-5-18".to_owned()];
            expected.sort();
            assert_eq!(trustees, expected);
        }
    }
}
