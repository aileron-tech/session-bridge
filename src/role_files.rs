//! The roles folder (REQ-037): `<state>/roles/<name>.md`, one file per role,
//! edited on the admin page, on the phone or in any editor. Reads regular
//! files only and never follows a symbolic link. A background poll every
//! 2 s reads the files whose listing changed and replaces the book's roles.
use crate::{
    platform,
    roles::{self, FileKind, FileProblem, Listing, ReadFile, Role, RoleBook, Stamp},
    store,
};
use anyhow::{Context, Result};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{Arc, Weak},
    time::Duration,
};
use tokio::sync::Mutex as AsyncMutex;

/// How often the folder is listed for changes.
pub const POLL: Duration = Duration::from_secs(2);
/// A role prompt's limit, as for any message.
const MAX_BYTES: u64 = 16_000;

#[derive(Debug, Clone)]
pub struct RoleFolder {
    dir: PathBuf,
}

/// A read of the whole folder: the roles, what to show about the files, and
/// the listing it was read from.
#[derive(Debug, Clone, Default)]
pub struct Scan {
    pub roles: Vec<Role>,
    pub view: roles::FolderView,
    pub listing: Listing,
}

impl RoleFolder {
    pub fn open(dir: PathBuf) -> Result<Self> {
        platform::ensure_private_dir(&dir)?;
        Ok(Self { dir })
    }
    pub fn dir(&self) -> &Path {
        &self.dir
    }
    pub fn path(&self, role: &str) -> PathBuf {
        self.dir.join(roles::file_name(role))
    }
    /// The role files, as the directory lists them.
    pub fn list(&self) -> Result<Listing> {
        let mut listing = Listing::new();
        for entry in fs::read_dir(&self.dir)
            .with_context(|| format!("Could not list {}", self.dir.display()))?
        {
            let entry = entry?;
            let Some(file) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if roles::role_of_file(&file).is_none() {
                continue;
            }
            // symlink_metadata: a link is listed as a link, never followed.
            let Ok(meta) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            let kind = if meta.file_type().is_symlink() {
                FileKind::Symlink
            } else if meta.is_file() {
                FileKind::Regular
            } else {
                FileKind::Other
            };
            listing.insert(
                file,
                Stamp {
                    kind,
                    len: meta.len(),
                    modified: meta.modified().ok(),
                },
            );
        }
        Ok(listing)
    }
    /// One file's role and content version.
    pub fn read(&self, file: &str, stamp: &Stamp) -> Result<(Role, String), FileProblem> {
        match stamp.kind {
            FileKind::Symlink => return Err(FileProblem::Symlink),
            FileKind::Other => return Err(FileProblem::NotRegular),
            FileKind::Regular => {}
        }
        let name = roles::role_of_file(file).ok_or(FileProblem::Name)?;
        let bytes = self
            .read_bytes(&self.dir.join(file))
            .map_err(|e| FileProblem::Unreadable(format!("{e:#}")))?
            .ok_or(FileProblem::Prompt)?;
        let role = roles::role_from_file(name, &bytes)?;
        Ok((role, roles::version(&bytes)))
    }
    /// A regular file's bytes, `None` when over the limit.
    fn read_bytes(&self, path: &Path) -> Result<Option<Vec<u8>>> {
        platform::verify_user_file(path)?;
        let mut bytes = Vec::new();
        fs::File::open(path)?
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)?;
        Ok((bytes.len() as u64 <= MAX_BYTES).then_some(bytes))
    }
    /// The version of the role's file as it is on disk now; `None` when
    /// there is no regular file.
    pub fn version_on_disk(&self, role: &str) -> Result<Option<String>> {
        let path = self.path(role);
        match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
            Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => Ok(None),
            Ok(_) => Ok(Some(match self.read_bytes(&path)? {
                Some(bytes) => roles::version(&bytes),
                None => String::from("over the limit"),
            })),
        }
    }
    pub fn exists(&self, role: &str) -> bool {
        fs::symlink_metadata(self.path(role)).is_ok()
    }
    /// Writes the role's file atomically: a link in its place is replaced,
    /// not followed.
    pub fn write(&self, role: &Role) -> Result<()> {
        store::write_bytes(&self.path(&role.name), role.prompt.as_bytes())
    }
    /// Renames the role `old`'s file to `role`'s name and writes its prompt.
    pub fn rename(&self, old: &str, role: &Role) -> Result<()> {
        if old.eq_ignore_ascii_case(&role.name) {
            // Only the case changes: on a case-insensitive disk the two
            // names are one file, so it is renamed rather than removed.
            if old != role.name && self.exists(old) {
                fs::rename(self.path(old), self.path(&role.name))?;
            }
            return self.write(role);
        }
        self.write(role)?;
        self.remove(old)
    }
    pub fn remove(&self, role: &str) -> Result<()> {
        match fs::remove_file(self.path(role)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }
    /// Reads the files of `listing`, reusing `last`'s reads of unchanged ones.
    pub fn scan(&self, listing: Listing, last: &Scan, reads: &mut Reads) -> Scan {
        let changed = roles::changed_files(&last.listing, &listing);
        reads.retain(|file, _| listing.contains_key(file));
        for file in &changed {
            reads.insert(file.clone(), self.read(file, &listing[file]));
        }
        let files: Vec<ReadFile> = reads
            .iter()
            .map(|(file, read)| (file.clone(), read.clone()))
            .collect();
        let (roles, view) = roles::assemble(files);
        Scan {
            roles,
            view,
            listing,
        }
    }
}

/// Each file's last read, by file name.
pub type Reads = BTreeMap<String, Result<(Role, String), FileProblem>>;

/// The book with the folder's roles: the built-in root when root.md is
/// missing or not loaded.
pub fn apply(book: &mut RoleBook, scan: &Scan) {
    book.roles = scan.roles.clone();
    book.files = scan.view.clone();
    let root = std::mem::take(book).with_root();
    *book = root;
}

/// Writes the built-in root prompt when root.md is missing; a root.md that
/// is there but not loaded stays as the user left it.
pub fn restore_root(folder: &RoleFolder) -> Result<()> {
    if !folder.exists(crate::spawn::ROOT) {
        folder.write(&Role {
            name: crate::spawn::ROOT.into(),
            prompt: crate::spawn::ROOT_PROMPT.into(),
        })?;
    }
    Ok(())
}

/// Polls the folder every 2 s on its own thread, off the event loop, until
/// the book is dropped. A change is applied under the book's lock only if
/// the folder still lists as read, so a save from the page (made under the
/// same lock) is never replaced by an older read.
pub fn watch(
    folder: RoleFolder,
    book: Weak<AsyncMutex<RoleBook>>,
    mut last: Scan,
    mut reads: Reads,
) {
    let spawned = std::thread::Builder::new()
        .name("roles-folder".into())
        .spawn(move || {
            let mut failure = String::new();
            loop {
                std::thread::sleep(POLL);
                let Some(book) = book.upgrade() else { return };
                match poll(&folder, &book, &last, &mut reads) {
                    Ok(Some(next)) => {
                        if next.view.not_loaded != last.view.not_loaded {
                            for problem in &next.view.not_loaded {
                                eprintln!(
                                    "Role file {} not loaded: {}",
                                    problem.file, problem.reason
                                );
                            }
                        }
                        last = next;
                        failure.clear();
                    }
                    Ok(None) => failure.clear(),
                    Err(e) => {
                        let message = format!("{e:#}");
                        if message != failure {
                            eprintln!("Could not read the roles folder: {message}");
                            failure = message;
                        }
                    }
                }
            }
        });
    if let Err(e) = spawned {
        eprintln!("Could not watch the roles folder: {e}");
    }
}

/// One poll: `None` when nothing changed or a save raced the read.
fn poll(
    folder: &RoleFolder,
    book: &Arc<AsyncMutex<RoleBook>>,
    last: &Scan,
    reads: &mut Reads,
) -> Result<Option<Scan>> {
    let listing = folder.list()?;
    if listing == last.listing {
        return Ok(None);
    }
    if !listing.contains_key(&roles::file_name(crate::spawn::ROOT)) {
        restore_root(folder)?;
        return Ok(None);
    }
    let next = folder.scan(listing, last, reads);
    let mut book = book.blocking_lock();
    if folder.list()? != next.listing {
        // Changed again while being read: the next poll reads what changed
        // since `last` again, and keeps the other files' reads.
        return Ok(None);
    }
    apply(&mut book, &next);
    Ok(Some(next))
}

/// The folder on start: migrates roles still kept in `roles.json` to files
/// (never replacing a file), offers the preset roles once as files, writes
/// root.md when missing, and reads the folder. True when `book` changed
/// what `roles.json` must keep.
pub fn load(folder: &RoleFolder, book: &mut RoleBook) -> Result<(Scan, Reads, bool)> {
    let legacy = std::mem::take(&mut book.roles);
    for role in &legacy {
        if !folder.exists(&role.name) {
            folder.write(role)?;
        }
    }
    let before = book.presets.clone();
    let offered = std::mem::take(book).with_presets();
    *book = offered;
    for name in book.presets.difference(&before) {
        if let Some(role) = book.role(name)
            && !folder.exists(&role.name)
        {
            folder.write(role)?;
        }
    }
    restore_root(folder)?;
    let mut reads = Reads::new();
    let scan = folder.scan(folder.list()?, &Scan::default(), &mut reads);
    for problem in &scan.view.not_loaded {
        eprintln!("Role file {} not loaded: {}", problem.file, problem.reason);
    }
    apply(book, &scan);
    Ok((scan, reads, !legacy.is_empty() || book.presets != before))
}

/// Shows the folder in the system's file manager, for the user at the
/// admin page only (never the control socket, MCP or a phone).
pub async fn open_folder(dir: &Path) -> Result<()> {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    let status = tokio::process::Command::new(program)
        .arg(dir)
        .status()
        .await
        .with_context(|| format!("Could not run {program}"))?;
    // Explorer reports 1 even when it opened the folder.
    anyhow::ensure!(
        status.success() || cfg!(windows),
        "{program} could not open {}",
        dir.display()
    );
    Ok(())
}

#[cfg(all(test, unix))]
#[path = "role_files_tests.rs"]
mod tests;
