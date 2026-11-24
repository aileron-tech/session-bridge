use anyhow::{Context, Result};
use std::{
    collections::BTreeMap,
    ffi::c_void,
    fs::{self, OpenOptions},
    io,
    os::windows::io::{AsRawHandle, IntoRawHandle},
    path::{Path, PathBuf},
    ptr::null_mut,
    time::Duration,
};
use tokio::{
    net::windows::named_pipe::{ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions},
    signal::windows::{CtrlBreak, ctrl_break},
    time::sleep,
};
use uuid::Uuid;
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_PIPE_BUSY, HANDLE, INVALID_HANDLE_VALUE, LocalFree, STILL_ACTIVE,
    },
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            SDDL_REVISION_1,
        },
        GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    System::{
        Console::{FreeConsole, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle},
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
            TH32CS_SNAPPROCESS,
        },
        Pipes::GetNamedPipeServerProcessId,
        Threading::{
            GetCurrentProcess, GetExitCodeProcess, OpenProcess, OpenProcessToken,
            PROCESS_QUERY_LIMITED_INFORMATION,
        },
    },
};

pub type ServerStream = NamedPipeServer;
pub type ClientStream = NamedPipeClient;

/// Closes a process, token, or snapshot handle on every return path.
struct Owned(HANDLE);
impl Drop for Owned {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
/// The string SID of the user that owns `process`.
fn process_user_sid(process: HANDLE) -> Result<String> {
    let mut token: HANDLE = null_mut();
    anyhow::ensure!(
        unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } != 0,
        "Cannot open process token: {}",
        io::Error::last_os_error()
    );
    let token = Owned(token);
    let mut length = 0;
    unsafe { GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut length) };
    anyhow::ensure!(length > 0, "Cannot size the process token user");
    // u64 storage keeps the TOKEN_USER header aligned.
    let mut buffer = vec![0u64; (length as usize).div_ceil(8)];
    anyhow::ensure!(
        unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                length,
                &mut length,
            )
        } != 0,
        "Cannot read the process token user: {}",
        io::Error::last_os_error()
    );
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut text: *mut u16 = null_mut();
    anyhow::ensure!(
        unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) } != 0,
        "Cannot format the user SID: {}",
        io::Error::last_os_error()
    );
    let mut size = 0;
    while unsafe { *text.add(size) } != 0 {
        size += 1;
    }
    let sid = String::from_utf16(unsafe { std::slice::from_raw_parts(text, size) });
    unsafe { LocalFree(text.cast()) };
    Ok(sid?)
}
pub fn current_user_sid() -> Result<String> {
    process_user_sid(unsafe { GetCurrentProcess() })
}

fn create_instance(name: &Path, first: bool) -> Result<NamedPipeServer> {
    // Protected DACL: only the current user may open the pipe.
    let sddl: Vec<u16> = format!("D:P(A;;GA;;;{})", current_user_sid()?)
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut descriptor: *mut c_void = null_mut();
    anyhow::ensure!(
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                null_mut(),
            )
        } != 0,
        "Cannot build the pipe security descriptor: {}",
        io::Error::last_os_error()
    );
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let server = unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(name, (&raw mut attributes).cast())
    };
    unsafe { LocalFree(descriptor) };
    server.with_context(|| format!("Cannot create named pipe {}", name.display()))
}
pub struct Listener {
    name: PathBuf,
    next: NamedPipeServer,
}
impl Listener {
    pub fn bind(path: &Path) -> Result<Self> {
        // first_pipe_instance fails when another process already owns the name.
        Ok(Self {
            name: path.to_owned(),
            next: create_instance(path, true)?,
        })
    }
    pub async fn accept(&mut self) -> Result<ServerStream> {
        self.next.connect().await?;
        let following = create_instance(&self.name, false)?;
        Ok(std::mem::replace(&mut self.next, following))
    }
}
/// A pipe name derived from the state directory, which is per user.
pub fn control_endpoint(state: &Path) -> PathBuf {
    let id = Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        state.to_string_lossy().to_lowercase().as_bytes(),
    );
    PathBuf::from(format!(r"\\.\pipe\session-bridge-{}", id.simple()))
}
/// The pipe DACL admits only the current user, so an accepted client is that user.
pub fn verify_peer(_stream: &ServerStream) -> Result<()> {
    Ok(())
}
pub fn verify_endpoint(_path: &Path) -> Result<()> {
    Ok(())
}
/// A pipe cannot be probed without consuming a connection; connecting decides.
pub fn endpoint_exists(_path: &Path) -> bool {
    true
}
/// Pipes disappear with their last handle: nothing stale remains on disk.
pub fn remove_stale_endpoint(_path: &Path) -> Result<()> {
    Ok(())
}
pub fn remove_endpoint(_path: &Path) -> Result<()> {
    Ok(())
}
pub async fn connect(path: &Path) -> Result<ClientStream> {
    let mut attempts = 0;
    let stream = loop {
        match ClientOptions::new().open(path) {
            Ok(stream) => break stream,
            Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) && attempts < 50 => {
                attempts += 1;
                sleep(Duration::from_millis(100)).await;
            }
            Err(error) => return Err(error.into()),
        }
    };
    // Reject a pipe created under this name by another user.
    let mut server = 0;
    anyhow::ensure!(
        unsafe { GetNamedPipeServerProcessId(stream.as_raw_handle(), &mut server) } != 0,
        "Cannot identify the pipe server: {}",
        io::Error::last_os_error()
    );
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, server) };
    anyhow::ensure!(!process.is_null(), "Cannot open the pipe server process");
    let process = Owned(process);
    anyhow::ensure!(
        process_user_sid(process.0)? == current_user_sid()?,
        "Pipe server is not the current user"
    );
    Ok(stream)
}

/// The state directory lives in the user profile and inherits its access
/// control list; reject anything that is not a real directory.
pub fn ensure_private_dir(path: &Path) -> Result<()> {
    if !path.exists() {
        fs::DirBuilder::new().recursive(true).create(path)?;
    }
    let meta = fs::symlink_metadata(path)?;
    anyhow::ensure!(
        meta.is_dir() && !meta.file_type().is_symlink(),
        "Not a real directory: {}",
        path.display()
    );
    Ok(())
}
pub fn verify_user_file(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    anyhow::ensure!(
        meta.is_file() && !meta.file_type().is_symlink(),
        "Not a regular file: {}",
        path.display()
    );
    Ok(())
}
/// Files inherit the access control list of the private state directory.
pub fn private_options() -> OpenOptions {
    OpenOptions::new()
}
pub fn set_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}
/// Windows offers no directory flush; the rename itself is journaled.
pub fn sync_dir(_path: &Path) -> io::Result<()> {
    Ok(())
}

pub fn is_alive(pid: u32) -> bool {
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return false;
    }
    let process = Owned(process);
    let mut code = 0;
    unsafe { GetExitCodeProcess(process.0, &mut code) != 0 && code == STILL_ACTIVE as u32 }
}
pub async fn ancestor_pids() -> Result<Vec<u32>> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    anyhow::ensure!(
        snapshot != INVALID_HANDLE_VALUE,
        "Cannot list processes: {}",
        io::Error::last_os_error()
    );
    let snapshot = Owned(snapshot);
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut parents = BTreeMap::new();
    let mut more = unsafe { Process32FirstW(snapshot.0, &mut entry) } != 0;
    while more {
        parents.insert(entry.th32ProcessID, entry.th32ParentProcessID);
        more = unsafe { Process32NextW(snapshot.0, &mut entry) } != 0;
    }
    let mut pid = std::process::id();
    let mut ancestors = Vec::new();
    for _ in 0..12 {
        match parents.get(&pid) {
            Some(parent) if *parent > 4 && !ancestors.contains(parent) => {
                ancestors.push(*parent);
                pid = *parent;
            }
            _ => break,
        }
    }
    Ok(ancestors)
}

/// Resolves when the console asks the process to stop.
pub struct Terminate(CtrlBreak);
impl Terminate {
    pub fn new() -> Result<Self> {
        Ok(Self(ctrl_break()?))
    }
    pub async fn recv(&mut self) {
        self.0.recv().await;
    }
}
/// Send output to `log` and release the console window that Task Scheduler
/// opens for a console program.
pub fn detach_to_log(log: &Path) -> Result<()> {
    let file = OpenOptions::new().create(true).append(true).open(log)?;
    let handle = file.into_raw_handle();
    unsafe {
        anyhow::ensure!(
            SetStdHandle(STD_OUTPUT_HANDLE, handle) != 0
                && SetStdHandle(STD_ERROR_HANDLE, handle) != 0,
            "Cannot redirect output: {}",
            io::Error::last_os_error()
        );
        FreeConsole();
    }
    Ok(())
}
