//! Worker confinement on Windows: an AppContainer (no access to the user's files, registry or loopback services except
//! folders explicitly granted) inside a Job Object (kill-on-close, process and memory caps). This is far below VM-grade
//! isolation: it stops a misbehaving worker from touching anything outside its scratch folder, not a determined attacker.
use std::ffi::c_void;
use std::fs::File;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use crate::agent::Access;
use std::path::Path;
use std::sync::OnceLock;
use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT};
use windows_sys::Win32::Security::Authorization::{ConvertSidToStringSidW, ConvertStringSidToSidW};
use windows_sys::Win32::Security::Isolation::{CreateAppContainerProfile, DeriveAppContainerSidFromAppContainerName};
use windows_sys::Win32::Security::{FreeSid, SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES};
use windows_sys::Win32::System::JobObjects::{AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{CreateProcessW, DeleteProcThreadAttributeList, InitializeProcThreadAttributeList, ResumeThread, UpdateProcThreadAttribute, PROCESS_INFORMATION, STARTUPINFOEXW, STARTUPINFOW};

const APP_CONTAINER: &str = "Wallis.Worker";
const ALREADY_EXISTS: i32 = 0x800700B7u32 as i32;
const PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES: usize = 0x0002_0009;
const SE_GROUP_ENABLED: u32 = 4;
const STARTF_USESTDHANDLES: u32 = 0x100;
const EXTENDED_STARTUPINFO_PRESENT: u32 = 0x0008_0000;
const CREATE_UNICODE_ENVIRONMENT: u32 = 0x0000_0400;
const CREATE_SUSPENDED: u32 = 4;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const DETACHED_PROCESS: u32 = 0x0000_0008;
const JOB_EXTENDED_LIMITS: i32 = 9;
const LIMIT_ACTIVE_PROCESS: u32 = 0x8;
const LIMIT_JOB_MEMORY: u32 = 0x200;
const LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;
const MAX_PROCESSES: u32 = 64;
const MAX_MEMORY: usize = 6 << 30;
/// "internetClient": outbound network, which the worker needs to reach its model provider.
const INTERNET_CLIENT: &str = "S-1-15-3-1";
/// "ALL APPLICATION PACKAGES": lets any AppContainer read, used for the runtime folders it must execute from.
const ALL_APP_PACKAGES: &str = "S-1-15-2-1";

fn wide(text: &str) -> Vec<u16> { text.encode_utf16().chain(std::iter::once(0)).collect() }

fn sid_string(sid: *mut c_void) -> Result<String, String> {
    let mut text = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 { return Err("Unable to read the sandbox identity.".into()); }
    let length = (0..).take_while(|&i| unsafe { *text.add(i) } != 0).count();
    let value = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) });
    unsafe { LocalFree(text as *mut c_void) };
    Ok(value)
}

/// The container's SID as a string, creating the profile the first time.
fn container_sid() -> Result<String, String> {
    let name = wide(APP_CONTAINER);
    let mut sid: *mut c_void = std::ptr::null_mut();
    let created = unsafe { CreateAppContainerProfile(name.as_ptr(), name.as_ptr(), name.as_ptr(), std::ptr::null(), 0, &mut sid) };
    if created == ALREADY_EXISTS {
        if unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid) } < 0 { return Err("Unable to find the worker sandbox.".into()); }
    } else if created < 0 { return Err(format!("Unable to create the worker sandbox (0x{created:08x}).")); }
    let text = sid_string(sid);
    unsafe { FreeSid(sid) };
    text
}

fn icacls(path: &Path, grant: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let output = std::process::Command::new("icacls").arg(path).arg("/grant").arg(grant).creation_flags(CREATE_NO_WINDOW).output().map_err(|error| format!("Unable to run icacls: {error}"))?;
    if output.status.success() { Ok(()) } else { Err(format!("Unable to grant sandbox access to {}: {}", path.display(), String::from_utf8_lossy(&output.stdout).trim())) }
}


fn grant(access: &Access, sid: &str) -> Result<(), String> {
    for dir in &access.read { icacls(dir, &format!("*{ALL_APP_PACKAGES}:(OI)(CI)(RX)"))?; }
    for dir in &access.write { icacls(dir, &format!("*{sid}:(OI)(CI)(M)"))?; }
    Ok(())
}

pub struct Sandboxed { pub stdin: File, pub stdout: File, job: HANDLE, process: HANDLE }

// The job handle is only ever used to terminate and close it.
unsafe impl Send for Sandboxed {}

impl Sandboxed {
    /// The worker's exit code once it has exited, `None` while it runs.
    pub fn exit_code(&self) -> Option<u32> {
        let mut code = 0u32;
        let ok = unsafe { windows_sys::Win32::System::Threading::GetExitCodeProcess(self.process, &mut code) } != 0;
        (ok && code != 259).then_some(code)
    }
}

impl Drop for Sandboxed {
    fn drop(&mut self) { unsafe { TerminateJobObject(self.job, 1); CloseHandle(self.job); CloseHandle(self.process); } }
}

fn quote(argument: &str) -> String { format!("\"{}\"", argument.replace('"', "\\\"")) }

fn pipe(inherit_read: bool) -> Result<(HANDLE, HANDLE), String> {
    let attrs = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: std::ptr::null_mut(), bInheritHandle: 1 };
    let (mut read, mut write): (HANDLE, HANDLE) = (std::ptr::null_mut(), std::ptr::null_mut());
    if unsafe { CreatePipe(&mut read, &mut write, &attrs, 0) } == 0 { return Err("Unable to create a pipe for the worker.".into()); }
    // The parent's end must not leak into the child.
    unsafe { SetHandleInformation(if inherit_read { write } else { read }, HANDLE_FLAG_INHERIT, 0) };
    Ok((read, write))
}

/// Starts `program` inside the sandbox with piped stdin/stdout. `env` is the child's entire environment.
pub fn spawn(program: &Path, args: &[String], cwd: &Path, env: &[(String, String)], access: &Access) -> Result<Sandboxed, String> {
    static GRANTED: OnceLock<Result<String, String>> = OnceLock::new();
    let sid_text = GRANTED.get_or_init(|| { let sid = container_sid()?; grant(access, &sid)?; Ok(sid) }).clone()?;
    let (mut sid, mut internet): (*mut c_void, *mut c_void) = (std::ptr::null_mut(), std::ptr::null_mut());
    unsafe {
        if ConvertStringSidToSidW(wide(&sid_text).as_ptr(), &mut sid) == 0 || ConvertStringSidToSidW(wide(INTERNET_CLIENT).as_ptr(), &mut internet) == 0 { return Err("Unable to build the sandbox identity.".into()); }
    }
    let mut capability = SID_AND_ATTRIBUTES { Sid: internet, Attributes: SE_GROUP_ENABLED };
    let mut security = SECURITY_CAPABILITIES { AppContainerSid: sid, Capabilities: &mut capability, CapabilityCount: 1, Reserved: 0 };

    let (stdin_read, stdin_write) = pipe(true)?;
    let (stdout_read, stdout_write) = pipe(false)?;
    let nul = std::fs::OpenOptions::new().write(true).open("NUL").map_err(|error| error.to_string())?;
    unsafe { SetHandleInformation(nul.as_raw_handle() as HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };

    let mut size = 0usize;
    unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size) };
    let mut list = vec![0u8; size];
    let list_ptr = list.as_mut_ptr() as *mut c_void;
    if unsafe { InitializeProcThreadAttributeList(list_ptr, 1, 0, &mut size) } == 0
        || unsafe { UpdateProcThreadAttribute(list_ptr, 0, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, &mut security as *mut _ as *mut c_void, std::mem::size_of::<SECURITY_CAPABILITIES>(), std::ptr::null_mut(), std::ptr::null()) } == 0 {
        return Err("Unable to configure the worker sandbox.".into());
    }
    let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    startup.StartupInfo = STARTUPINFOW { cb: std::mem::size_of::<STARTUPINFOEXW>() as u32, dwFlags: STARTF_USESTDHANDLES, hStdInput: stdin_read, hStdOutput: stdout_write, hStdError: nul.as_raw_handle() as HANDLE, ..unsafe { std::mem::zeroed() } };
    startup.lpAttributeList = list_ptr;

    let mut command = wide(&std::iter::once(program.to_string_lossy().into_owned()).chain(args.iter().cloned()).map(|a| quote(&a)).collect::<Vec<_>>().join(" "));
    // Windows wants the block sorted case-insensitively by name; an empty list inherits the parent's environment.
    // An AppContainer process fails to start (error 203) without LOCALAPPDATA, so default it to the writable cwd.
    let fallback = ("LOCALAPPDATA".to_string(), cwd.to_string_lossy().into_owned());
    let mut sorted: Vec<&(String, String)> = env.iter().collect();
    if !env.iter().any(|(key, _)| key.eq_ignore_ascii_case("LOCALAPPDATA")) { sorted.push(&fallback); }
    sorted.sort_by_key(|(key, _)| key.to_uppercase());
    let mut block: Vec<u16> = sorted.iter().flat_map(|(key, value)| wide(&format!("{key}={value}"))).collect();
    block.push(0);
    let environment = if sorted.is_empty() { std::ptr::null() } else { block.as_ptr() as *const c_void };
    let (program_w, cwd_w) = (wide(&program.to_string_lossy()), wide(&cwd.to_string_lossy()));
    let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let created = unsafe { CreateProcessW(program_w.as_ptr(), command.as_mut_ptr(), std::ptr::null(), std::ptr::null(), 1, EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED | DETACHED_PROCESS, environment, cwd_w.as_ptr(), &startup.StartupInfo as *const STARTUPINFOW as *const _, &mut info) };
    let error = std::io::Error::last_os_error();
    unsafe {
        DeleteProcThreadAttributeList(list_ptr);
        CloseHandle(stdin_read);
        CloseHandle(stdout_write);
        LocalFree(sid);
        LocalFree(internet);
    }
    if created == 0 {
        unsafe { CloseHandle(stdin_write); CloseHandle(stdout_read); }
        return Err(format!("Unable to start the sandboxed worker: {error}"));
    }

    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = LIMIT_KILL_ON_JOB_CLOSE | LIMIT_ACTIVE_PROCESS | LIMIT_JOB_MEMORY;
    limits.BasicLimitInformation.ActiveProcessLimit = MAX_PROCESSES;
    limits.JobMemoryLimit = MAX_MEMORY;
    let confined = unsafe { SetInformationJobObject(job, JOB_EXTENDED_LIMITS, &limits as *const _ as *const c_void, std::mem::size_of_val(&limits) as u32) != 0 && AssignProcessToJobObject(job, info.hProcess) != 0 };
    if !confined {
        unsafe { terminate_process(info.hProcess); CloseHandle(info.hProcess); CloseHandle(info.hThread); CloseHandle(job); CloseHandle(stdin_write); CloseHandle(stdout_read); }
        return Err("Unable to place the worker in its job object.".into());
    }
    unsafe { ResumeThread(info.hThread); CloseHandle(info.hThread); }
    Ok(Sandboxed { stdin: unsafe { File::from_raw_handle(stdin_write as _) }, stdout: unsafe { File::from_raw_handle(stdout_read as _) }, job, process: info.hProcess })
}

unsafe fn terminate_process(process: HANDLE) { windows_sys::Win32::System::Threading::TerminateProcess(process, 1); }
