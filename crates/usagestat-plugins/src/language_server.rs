//! Bounded, same-user IDE discovery. Never log command lines or CSRF tokens.
use serde::{Deserialize, Serialize};
#[cfg(windows)]
use std::path::PathBuf;
use std::time::Duration;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::time::Instant;
use usagestat_core::process;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Request {
    pub process_name: String,
    #[serde(default)]
    pub markers: Vec<String>,
    #[serde(default)]
    pub csrf_flag: Option<String>,
    #[serde(default)]
    pub port_flag: Option<String>,
    #[serde(default)]
    pub pid: Option<u32>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Response {
    pub csrf: String,
    pub ports: Vec<u16>,
    pub extension_port: Option<u16>,
    pub pid: u32,
}
pub(crate) struct Candidate {
    pid: u32,
    executable: String,
    args: Vec<String>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Report {
    pub status: &'static str,
    pub reason_code: Option<&'static str>,
    pub result: Option<Response>,
}
impl Report {
    fn unavailable(status: &'static str, reason: &'static str) -> Self {
        Self {
            status,
            reason_code: Some(reason),
            result: None,
        }
    }
}

pub(crate) fn discover(request: &Request) -> Report {
    if request.process_name.is_empty()
        || request.process_name.len() > 128
        || !request
            .process_name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        || request.markers.len() > 16
        || request
            .markers
            .iter()
            .any(|m| m.len() > 1024 || m.contains('\0'))
    {
        return Report::unavailable("invalid", "ide-discovery-invalid-request");
    }
    // The older agy presence-only request has no endpoint flags. It cannot
    // yield an authenticated local server, so retain its null result.
    if request.csrf_flag.as_deref().is_none_or(str::is_empty)
        || request.port_flag.as_deref().is_none_or(str::is_empty)
    {
        return Report::unavailable("missing", "ide-endpoint-flags-missing");
    }
    let candidates = match native_candidates(&request.process_name) {
        Ok(candidates) => candidates,
        Err(reason) => {
            return Report::unavailable(
                if reason == "ide-discovery-unsupported" {
                    "unsupported"
                } else {
                    "unavailable"
                },
                reason,
            );
        }
    };
    select(request, candidates)
}

fn executable_matches(executable: &str, requested: &str) -> bool {
    let name = executable
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(executable)
        .to_ascii_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    let requested = requested.to_ascii_lowercase();
    name == requested
        || name
            .strip_prefix(&requested)
            .is_some_and(|tail| tail.starts_with(['_', '-']))
}

fn flag(args: &[String], name: &str) -> Option<String> {
    if !name.starts_with("--") || name.contains(['=', ' ', '\0']) {
        return None;
    }
    let mut values = Vec::new();
    for (index, arg) in args.iter().enumerate().skip(1) {
        if arg == name {
            values.push(args.get(index + 1)?.clone());
        } else if let Some(value) = arg.strip_prefix(&format!("{name}=")) {
            values.push(value.to_owned());
        }
    }
    // Duplicate flags can be interpreted differently by different runtimes.
    // Fail closed instead of choosing a potentially unrelated token or port.
    (values.len() == 1).then(|| values.remove(0))
}

fn select(request: &Request, mut candidates: Vec<Candidate>) -> Report {
    candidates.sort_by_key(|candidate| candidate.pid);
    let mut matches = Vec::new();
    for candidate in candidates {
        if request.pid.is_some_and(|pid| pid != candidate.pid)
            || !executable_matches(&candidate.executable, &request.process_name)
            || !request
                .markers
                .iter()
                .filter(|marker| !marker.is_empty())
                .all(|marker| candidate.args.iter().any(|arg| arg.contains(marker)))
        {
            continue;
        }
        let Some(csrf) = request
            .csrf_flag
            .as_deref()
            .and_then(|name| flag(&candidate.args, name))
        else {
            continue;
        };
        if csrf.is_empty() || csrf.len() > 4096 || csrf.chars().any(char::is_control) {
            continue;
        }
        let Some(port) = request
            .port_flag
            .as_deref()
            .and_then(|name| flag(&candidate.args, name))
            .and_then(|value| value.parse::<u16>().ok())
            .filter(|port| *port != 0)
        else {
            continue;
        };
        matches.push(Response {
            csrf,
            ports: vec![port],
            extension_port: Some(port),
            pid: candidate.pid,
        });
    }
    if matches.len() > 1 {
        return Report::unavailable("ambiguous", "ide-multiple-instances-select-pid");
    }
    match matches.pop() {
        Some(result) => Report {
            status: "ready",
            reason_code: None,
            result: Some(result),
        },
        None => Report::unavailable("missing", "ide-process-or-endpoint-missing"),
    }
}

#[cfg(target_os = "linux")]
fn native_candidates(name: &str) -> Result<Vec<Candidate>, &'static str> {
    use std::{io::Read, os::unix::fs::MetadataExt};
    let deadline = Instant::now() + Duration::from_secs(3);
    let uid = unsafe { libc::geteuid() };
    let mut candidates = Vec::new();
    for entry in std::fs::read_dir("/proc")
        .map_err(|_| "ide-process-query-denied")?
        .flatten()
    {
        if Instant::now() >= deadline
            || process::current_cancellation().is_some_and(|token| token.is_cancelled())
        {
            return Err("ide-discovery-timed-out");
        }
        let Ok(pid) = entry.file_name().to_string_lossy().parse() else {
            continue;
        };
        if !entry.metadata().is_ok_and(|metadata| metadata.uid() == uid) {
            continue;
        }
        let Ok(executable) = std::fs::read_link(entry.path().join("exe")) else {
            continue;
        };
        let executable = executable.to_string_lossy().into_owned();
        if !executable_matches(&executable, name) {
            continue;
        }
        let Ok(file) = std::fs::File::open(entry.path().join("cmdline")) else {
            continue;
        };
        let mut bytes = Vec::new();
        if file
            .take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .is_err()
            || bytes.len() > 4 * 1024 * 1024
        {
            continue;
        }
        let Some(args) = nul_arguments(&bytes) else {
            continue;
        };
        candidates.push(Candidate {
            pid,
            executable,
            args,
        });
    }
    Ok(candidates)
}

#[cfg(any(target_os = "linux", test))]
fn nul_arguments(bytes: &[u8]) -> Option<Vec<String>> {
    if bytes.last() != Some(&0) {
        return None;
    }
    bytes[..bytes.len() - 1]
        .split(|byte| *byte == 0)
        .map(|arg| String::from_utf8(arg.to_vec()).ok())
        .collect()
}

#[cfg(target_os = "macos")]
fn native_candidates(name: &str) -> Result<Vec<Candidate>, &'static str> {
    let mut command = process::command("/bin/ps").map_err(|_| "ide-process-query-unavailable")?;
    command.args(["-ww", "-axo", "pid=,uid=,comm="]);
    let output = process::run(command, Duration::from_secs(3), 2 * 1024 * 1024)
        .map_err(|_| "ide-discovery-timed-out")?;
    if !output.status.success() {
        return Err("ide-process-query-denied");
    }
    let uid = unsafe { libc::geteuid() };
    let deadline = Instant::now() + Duration::from_secs(5);
    // KERN_PROCARGS2 rejects a supplied buffer larger than KERN_ARGMAX,
    // even if the process itself has only a few arguments.
    let mut limits = [libc::CTL_KERN, libc::KERN_ARGMAX];
    let mut argmax: libc::c_int = 0;
    let mut limit_size = std::mem::size_of_val(&argmax);
    let queried = unsafe {
        libc::sysctl(
            limits.as_mut_ptr(),
            2,
            (&mut argmax as *mut libc::c_int).cast(),
            &mut limit_size,
            std::ptr::null_mut(),
            0,
        )
    };
    if queried != 0
        || limit_size != std::mem::size_of_val(&argmax)
        || !(1..=4 * 1024 * 1024).contains(&argmax)
    {
        return Err("ide-process-query-unavailable");
    }
    let mut candidates = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if Instant::now() >= deadline
            || process::current_cancellation().is_some_and(|token| token.is_cancelled())
        {
            return Err("ide-discovery-timed-out");
        }
        let line = line.trim_start();
        let Some((pid, rest)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let Some((owner, executable)) = rest.trim_start().split_once(char::is_whitespace) else {
            continue;
        };
        let (Ok(pid), Ok(owner)) = (pid.parse::<u32>(), owner.parse::<u32>()) else {
            continue;
        };
        if owner != uid || !executable_matches(executable.trim(), name) {
            continue;
        }
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as i32];
        let mut buffer = vec![0u8; argmax as usize];
        let mut length = buffer.len();
        let result = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                buffer.as_mut_ptr().cast(),
                &mut length,
                std::ptr::null_mut(),
                0,
            )
        };
        if result != 0 || length > buffer.len() {
            continue;
        }
        if let Some(args) = mac_arguments(&buffer[..length]) {
            candidates.push(Candidate {
                pid,
                executable: executable.trim().to_owned(),
                args,
            });
        }
    }
    Ok(candidates)
}

#[cfg(any(target_os = "macos", test))]
fn mac_arguments(bytes: &[u8]) -> Option<Vec<String>> {
    let argc = i32::from_ne_bytes(bytes.get(..4)?.try_into().ok()?);
    if !(1..=32768).contains(&argc) {
        return None;
    }
    let mut cursor = 4 + bytes.get(4..)?.iter().position(|byte| *byte == 0)?;
    while bytes.get(cursor) == Some(&0) {
        cursor += 1;
    }
    let mut args = Vec::new();
    for _ in 0..argc {
        let end = cursor + bytes.get(cursor..)?.iter().position(|byte| *byte == 0)?;
        args.push(String::from_utf8(bytes[cursor..end].to_vec()).ok()?);
        cursor = end + 1;
    }
    Some(args)
}

#[cfg(windows)]
fn native_candidates(name: &str) -> Result<Vec<Candidate>, &'static str> {
    use base64::Engine;
    let system = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or("ide-discovery-unsupported")?;
    let powershell = system.join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let mut command = process::command(powershell).map_err(|_| "ide-discovery-unsupported")?;
    // Inputs are fixed code plus a strict ASCII process-name grammar validated
    // above. No profile, user script, WMIC, localized table or administrator token.
    let script = format!(
        r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$owner = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$rows = @()
Get-CimInstance -ClassName Win32_Process -Filter "Name LIKE '{name}%'" -OperationTimeoutSec 2 | ForEach-Object {{
  try {{
    $sid = Invoke-CimMethod -InputObject $_ -MethodName GetOwnerSid -OperationTimeoutSec 2
    if ($sid.ReturnValue -eq 0 -and $sid.Sid -eq $owner -and $_.CommandLine) {{
      $rows += @{{ pid = $_.ProcessId; executable = $_.Name; commandLine = $_.CommandLine }}
    }}
  }} catch {{ }}
}}
ConvertTo-Json -InputObject @($rows) -Compress -Depth 3
"#
    );
    let encoded = base64::engine::general_purpose::STANDARD.encode(
        script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    command.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-EncodedCommand",
        &encoded,
    ]);
    // Cold Windows PowerShell/CIM startup exceeded eight seconds on native
    // hosted runners. Keep per-operation CIM limits and a bounded total budget
    // below the normal provider probe deadline.
    let output = process::run(command, Duration::from_secs(15), 2 * 1024 * 1024)
        .map_err(|_| "ide-discovery-timed-out")?;
    if !output.status.success() {
        return Err("ide-process-query-denied");
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Row {
        pid: u32,
        executable: String,
        command_line: String,
    }
    let rows: Vec<Row> =
        serde_json::from_slice(&output.stdout).map_err(|_| "ide-process-query-invalid-response")?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            windows_arguments(&row.command_line).map(|args| Candidate {
                pid: row.pid,
                executable: row.executable,
                args,
            })
        })
        .collect())
}

/// CommandLineToArgvW-compatible backslash/quote rules, with malformed quoting
/// rejected. The process receives argv, so shell metacharacters stay literal.
#[cfg(any(windows, test))]
fn windows_arguments(command: &str) -> Option<Vec<String>> {
    let chars: Vec<char> = command.chars().collect();
    let (mut index, mut args) = (0, Vec::new());
    while index < chars.len() {
        while chars.get(index).is_some_and(|c| matches!(c, ' ' | '\t')) {
            index += 1;
        }
        if index == chars.len() {
            break;
        }
        let (mut arg, mut quoted) = (String::new(), false);
        while index < chars.len() {
            if !quoted && matches!(chars[index], ' ' | '\t') {
                break;
            }
            let mut slashes = 0;
            while chars.get(index) == Some(&'\\') {
                slashes += 1;
                index += 1;
            }
            if chars.get(index) == Some(&'"') {
                arg.extend(std::iter::repeat_n('\\', slashes / 2));
                if slashes % 2 == 1 {
                    arg.push('"');
                } else if quoted && chars.get(index + 1) == Some(&'"') {
                    arg.push('"');
                    index += 1;
                } else {
                    quoted = !quoted;
                }
                index += 1;
            } else {
                arg.extend(std::iter::repeat_n('\\', slashes));
                if index == chars.len() {
                    break;
                }
                if !quoted && matches!(chars[index], ' ' | '\t') {
                    break;
                }
                arg.push(chars[index]);
                index += 1;
            }
        }
        if quoted || arg.contains('\0') {
            return None;
        }
        args.push(arg);
    }
    Some(args)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn native_candidates(_: &str) -> Result<Vec<Candidate>, &'static str> {
    Err("ide-discovery-unsupported")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> Request {
        Request {
            process_name: "language_server".into(),
            markers: vec!["fixture-ide".into()],
            csrf_flag: Some("--csrf_token".into()),
            port_flag: Some("--port".into()),
            pid: None,
        }
    }
    fn candidate(pid: u32, port: &str) -> Candidate {
        Candidate {
            pid,
            executable: "/Applications/IDE 使用/language_server_macos_arm".into(),
            args: vec![
                "language_server".into(),
                "--data=/home/使用/fixture-ide data".into(),
                "--csrf_token".into(),
                "synthetic secret with spaces".into(),
                format!("--port={port}"),
            ],
        }
    }
    #[test]
    fn rejects_invalid_candidates_and_requires_an_explicit_choice_for_multiple_instances() {
        let mut req = request();
        let report = select(
            &req,
            vec![
                candidate(1, "0"),
                candidate(2, "65536"),
                candidate(3, "1234"),
            ],
        );
        assert_eq!(report.status, "ready");
        assert_eq!(report.result.unwrap().pid, 3);
        assert_eq!(
            select(&req, vec![candidate(2, "1234"), candidate(1, "1235")]).status,
            "ambiguous"
        );
        req.pid = Some(2);
        assert_eq!(
            select(&req, vec![candidate(2, "1234"), candidate(1, "1235")])
                .result
                .unwrap()
                .pid,
            2
        );
        let mut duplicate = candidate(2, "1234");
        duplicate.args.push("--port=1235".into());
        assert_eq!(select(&req, vec![duplicate]).status, "missing");
        req.markers = vec!["unrelated-ide".into()];
        assert_eq!(select(&req, vec![candidate(2, "1234")]).status, "missing");
        assert!(!executable_matches(
            "unrelated_language_server",
            "language_server"
        ));
    }
    #[test]
    fn reads_unix_argument_arrays_without_reparsing_quotes_spaces_or_environment() {
        let args =
            b"/IDE with spaces/language_server\0--csrf_token\0secret \"quoted\"\0--port=1234\0";
        assert_eq!(nul_arguments(args).unwrap()[2], "secret \"quoted\"");
        assert!(nul_arguments(b"truncated").is_none());
        let mut mac = 4i32.to_ne_bytes().to_vec();
        mac.extend_from_slice(b"/IDE with spaces/language_server\0\0\0");
        mac.extend_from_slice(args);
        mac.extend_from_slice(b"DO_NOT_PARSE=environment-secret\0");
        assert_eq!(mac_arguments(&mac), nul_arguments(args));
        assert!(mac_arguments(&[0, 0]).is_none());
    }
    #[test]
    fn windows_quoting_preserves_tokens_unicode_backslashes_and_empty_arguments() {
        let args = windows_arguments(r#""C:\IDE 使用\language_server.exe" --csrf_token "secret with \"quotes\"" --data C:\data\ --port=1234 """#).unwrap();
        assert_eq!(
            args,
            [
                "C:\\IDE 使用\\language_server.exe",
                "--csrf_token",
                "secret with \"quotes\"",
                "--data",
                "C:\\data\\",
                "--port=1234",
                ""
            ]
        );
        assert!(windows_arguments("\"unterminated").is_none());
    }
}
