use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use usagestat_core::{daemon_settings::DaemonSettings, storage};
use windows::Win32::Foundation::*;
use windows::Win32::System::JobObjects::*;
use windows::Win32::System::Threading::*;
use windows::core::{PCWSTR, PWSTR};

struct OwnedHandle(HANDLE);
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

struct Attributes {
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
    _memory: Vec<usize>,
}
impl Drop for Attributes {
    fn drop(&mut self) {
        unsafe {
            DeleteProcThreadAttributeList(self.list);
        }
    }
}

fn win_error(error: windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error(error.code().0 & 0xffff)
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain([0]).collect()
}

fn quote(argument: &str) -> String {
    let mut result = String::from("\"");
    let mut slashes = 0;
    for character in argument.chars() {
        if character == '\\' {
            slashes += 1;
            continue;
        }
        result.extend(std::iter::repeat_n(
            '\\',
            if character == '"' {
                slashes * 2 + 1
            } else {
                slashes
            },
        ));
        result.push(character);
        slashes = 0;
    }
    result.extend(std::iter::repeat_n('\\', slashes * 2));
    result.push('"');
    result
}

pub(super) fn run(settings: &Path) -> io::Result<u32> {
    // Do not put malformed settings values (which may contain credentials) in
    // launcher diagnostics. Preserve the OS error category instead.
    let configuration = DaemonSettings::load(settings)
        .map_err(|error| io::Error::new(error.kind(), "read saved daemon settings"))?
        .ok_or_else(|| invalid("saved daemon settings are missing"))?;
    let installation = configuration
        .installation
        .ok_or_else(|| invalid("daemon installation is not configured"))?;
    if !installation.binary.is_absolute() || installation.binary == std::env::current_exe()? {
        return Err(invalid(
            "saved backend executable must be an absolute, distinct path",
        ));
    }
    let data = installation
        .environment
        .get("USAGESTAT_DATA_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| invalid("saved local data directory is missing"))?;
    let logs = data.join("logs");
    storage::private_directory(&logs)?;
    let stdout = storage::open_private_append(&logs.join("daemon.stdout.log"))?;
    let stderr = storage::open_private_append(&logs.join("daemon.stderr.log"))?;
    let stdin = File::open("NUL")?;
    let handles = [
        HANDLE(stdin.as_raw_handle()),
        HANDLE(stdout.as_raw_handle()),
        HANDLE(stderr.as_raw_handle()),
    ];
    for handle in handles {
        unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT) }
            .map_err(win_error)?;
    }

    let job = OwnedHandle(unsafe { CreateJobObjectW(None, PCWSTR::null()) }.map_err(win_error)?);
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of_val(&limits) as u32,
        )
    }
    .map_err(win_error)?;
    let mut size = 0;
    unsafe {
        let _ = InitializeProcThreadAttributeList(None, 2, None, &mut size);
    }
    if size == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut memory = vec![0usize; size.div_ceil(size_of::<usize>())];
    let list = LPPROC_THREAD_ATTRIBUTE_LIST(memory.as_mut_ptr().cast());
    unsafe { InitializeProcThreadAttributeList(Some(list), 2, None, &mut size) }
        .map_err(win_error)?;
    let attributes = Attributes {
        list,
        _memory: memory,
    };
    unsafe {
        UpdateProcThreadAttribute(
            attributes.list,
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            Some(handles.as_ptr().cast()),
            size_of_val(&handles),
            None,
            None,
        )
        .map_err(win_error)?;
        // Job assignment is atomic with process creation. No child can start
        // before it belongs to the job that will die with this GUI launcher.
        UpdateProcThreadAttribute(
            attributes.list,
            0,
            PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
            Some((&job.0 as *const HANDLE).cast()),
            size_of::<HANDLE>(),
            None,
            None,
        )
        .map_err(win_error)?;
    }
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = handles[0];
    startup.StartupInfo.hStdOutput = handles[1];
    startup.StartupInfo.hStdError = handles[2];
    startup.lpAttributeList = attributes.list;
    let binary_text = installation
        .binary
        .to_str()
        .ok_or_else(|| invalid("backend path must be UTF-8"))?;
    let settings_text = settings
        .to_str()
        .ok_or_else(|| invalid("settings path must be UTF-8"))?;
    let command = format!(
        "{} --service-settings {}",
        quote(binary_text),
        quote(settings_text)
    );
    let mut command: Vec<u16> = command.encode_utf16().chain([0]).collect();
    let binary = wide(&installation.binary);
    let directory = wide(
        settings
            .parent()
            .ok_or_else(|| invalid("settings need a parent directory"))?,
    );
    let mut child = PROCESS_INFORMATION::default();
    unsafe {
        CreateProcessW(
            PCWSTR(binary.as_ptr()),
            Some(PWSTR(command.as_mut_ptr())),
            None,
            None,
            true,
            CREATE_NO_WINDOW | EXTENDED_STARTUPINFO_PRESENT,
            None,
            PCWSTR(directory.as_ptr()),
            &startup.StartupInfo,
            &mut child,
        )
    }
    .map_err(win_error)?;
    let process = OwnedHandle(child.hProcess);
    let _thread = OwnedHandle(child.hThread);
    if unsafe { WaitForSingleObject(process.0, INFINITE) } == WAIT_FAILED {
        return Err(io::Error::last_os_error());
    }
    let mut code = 1;
    unsafe { GetExitCodeProcess(process.0, &mut code) }.map_err(win_error)?;
    // Closing the non-inherited job handle also removes any descendants that
    // survived the backend. A forced Task Scheduler stop has the same effect.
    drop(job);
    Ok(code)
}
