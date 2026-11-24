use anyhow::{Context, Result};
use std::{
    fs::{self, File, OpenOptions},
    io,
    os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    net::{UnixListener, UnixStream},
    process::Command,
    signal::unix::{Signal, SignalKind, signal},
    time::timeout,
};

pub type ServerStream = UnixStream;
pub type ClientStream = UnixStream;

pub struct Listener(UnixListener);
impl Listener {
    pub fn bind(path: &Path) -> Result<Self> {
        let listener = UnixListener::bind(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        Ok(Self(listener))
    }
    pub async fn accept(&mut self) -> Result<ServerStream> {
        Ok(self.0.accept().await?.0)
    }
}
pub fn control_endpoint(state: &Path) -> PathBuf {
    state.join("control.sock")
}
pub fn verify_peer(stream: &UnixStream) -> Result<()> {
    #[cfg(target_os = "macos")]
    let uid = {
        use std::os::fd::AsRawFd;
        let mut uid = 0;
        let mut gid = 0;
        // Darwin retains UID/GID after close, but Tokio also requests a PID,
        // which fails with ENOTCONN once a one-way sender disconnects.
        if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        uid
    };
    #[cfg(not(target_os = "macos"))]
    let uid = stream.peer_cred()?.uid();
    anyhow::ensure!(
        uid == unsafe { libc::geteuid() },
        "Peer is not the current user"
    );
    Ok(())
}
pub fn verify_endpoint(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    anyhow::ensure!(
        meta.file_type().is_socket()
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.mode() & 0o077 == 0,
        "Not a private user-owned socket: {}",
        path.display()
    );
    Ok(())
}
pub fn endpoint_exists(path: &Path) -> bool {
    path.exists()
}
pub fn remove_stale_endpoint(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path).is_ok() {
        verify_endpoint(path)?;
        fs::remove_file(path)?;
    }
    Ok(())
}
pub fn remove_endpoint(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
pub async fn connect(path: &Path) -> Result<ClientStream> {
    verify_endpoint(path)?;
    let stream = timeout(Duration::from_secs(5), UnixStream::connect(path))
        .await
        .context("Socket connection timeout")??;
    verify_peer(&stream)?;
    Ok(stream)
}

pub fn ensure_private_dir(path: &Path) -> Result<()> {
    if !path.exists() {
        fs::DirBuilder::new().recursive(true).create(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    let meta = fs::symlink_metadata(path)?;
    anyhow::ensure!(
        meta.is_dir()
            && !meta.file_type().is_symlink()
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.mode() & 0o077 == 0,
        "Not a private user-owned directory: {}",
        path.display()
    );
    Ok(())
}
pub fn verify_user_file(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    anyhow::ensure!(
        meta.is_file()
            && !meta.file_type().is_symlink()
            && meta.uid() == unsafe { libc::geteuid() },
        "Not a user-owned regular file: {}",
        path.display()
    );
    Ok(())
}
/// Options for a file readable and writable by its owner only.
pub fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.mode(0o600);
    options
}
pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}
pub fn sync_dir(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

pub fn is_alive(pid: u32) -> bool {
    // kill(pid, 0) checks existence without delivering a signal.
    pid > 1 && unsafe { libc::kill(pid as i32, 0) } == 0
}
pub async fn ancestor_pids() -> Result<Vec<u32>> {
    let mut pid = std::process::id();
    let mut ancestors = Vec::new();
    for _ in 0..12 {
        let output = Command::new("/bin/ps")
            .args(["-p", &pid.to_string(), "-o", "ppid="])
            .output()
            .await?;
        if !output.status.success() {
            break;
        }
        pid = String::from_utf8(output.stdout)?.trim().parse()?;
        if pid <= 1 {
            break;
        }
        ancestors.push(pid);
    }
    Ok(ancestors)
}

/// Resolves when the service manager asks the process to stop.
pub struct Terminate(Signal);
impl Terminate {
    pub fn new() -> Result<Self> {
        Ok(Self(signal(SignalKind::terminate())?))
    }
    pub async fn recv(&mut self) {
        self.0.recv().await;
    }
}
