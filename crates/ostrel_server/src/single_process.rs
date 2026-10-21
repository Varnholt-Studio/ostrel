//! Start guard: one server process per app database (ARCHITECTURE 5.4 and 16 risk 10, D153,
//! D160, SPEC 12.14).
//!
//! In v0.3 exactly one server process serves an app database. Revocation of a device key
//! (D102), the broadcast order (D126) and the quiet point of snapshots (D151) live in the
//! memory of that process and are only correct under this rule. A second process on the
//! same database would keep sessions of a revoked key open, send ops outside the broadcast
//! order and read snapshots at a point that is not quiet for the other process.
//!
//! [`StartGuard`] enforces the rule on one host. A process takes the guard before it opens
//! the database, before it reads the log end and, on PostgreSQL, before the adapter ends the
//! leftover backends of the app (ARCHITECTURE 5.4 "Commit outcome", 6.1), and keeps it until
//! it exits. A second process on the same database fails to take it and must not start; the
//! error names the lock file and, when it can be read, the process that holds it.
//!
//! Mechanism: an exclusive advisory lock (`flock` on Linux) on a lock file. The operating
//! system releases it when the process ends in any way, also on a crash or `SIGKILL`, so there
//! is never a stale lock to clean up. The lock file itself is never removed: removing it
//! would let a later process lock a new file while an older one still holds the old inode.
//!
//! Where the lock file lies:
//! * `sqlite:<path>`: next to the database file, `<file>.lock`, after following symbolic
//!   links of the file and resolving its directory, so two spellings of one file (relative
//!   and absolute, through a symbolic link) share one lock.
//! * `postgres://` and `postgresql://`: in a lock directory the caller names, one file per
//!   database name, `ostrel-pg-<sha256 of the name>.lock`. The key is the database name
//!   alone, the same key the adapter uses for `application_name` (ARCHITECTURE 6.1), so two
//!   spellings of one server (`localhost`, `127.0.0.1`, a socket) cannot get around it. Two
//!   apps on one host whose databases have the same name on different PostgreSQL servers
//!   therefore refuse each other; they need different lock directories.
//! * `memory:` and `sqlite::memory:`: no guard, the database lives inside the process.
//!
//! Limits (D160, kira #2323): the guard only sees processes on the same host that share the
//! file system of the lock file. Processes on several hosts or in containers without a common
//! file system are out of scope for v0.3 and are not detected (`docs/sync.md`,
//! `docs/security.md`). Network file systems may not implement `flock` faithfully; a lock
//! file there is unsupported. A hard link to the database file under another name gets its
//! own lock file and is not detected either.

use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// URL scheme of the SQLite adapter.
const SQLITE: &str = "sqlite:";
/// The SQLite path that names an in memory database.
const SQLITE_MEMORY: &str = ":memory:";
/// URL scheme of the memory adapter.
const MEMORY: &str = "memory:";
/// URL schemes of PostgreSQL.
const POSTGRES: [&str; 2] = ["postgres://", "postgresql://"];
/// Suffix of the lock file next to a SQLite database file.
const LOCK_SUFFIX: &str = ".lock";
/// Symbolic links followed from a SQLite path before giving up (the limit of Linux).
const MAX_LINKS: usize = 40;
/// Attempts to lock a lock file that is replaced or removed while it is being locked.
const MAX_ATTEMPTS: usize = 3;
/// At most this many bytes of a lock file are read to find the process that holds it.
const MAX_PID_BYTES: u64 = 32;

/// Why a server process may not start on its database.
#[derive(Debug)]
pub enum StartError {
    /// Another process holds the start lock of the same app database.
    AlreadyRunning {
        /// The lock file.
        lock: PathBuf,
        /// The process id written by the holder, if it could be read.
        holder: Option<u32>,
    },
    /// The database URL has a scheme no adapter serves.
    UnsupportedUrl,
    /// A `sqlite:` URL without a path, or with a path that names no file.
    InvalidSqlitePath,
    /// A PostgreSQL URL that names no database; the guard does not guess the default name.
    PostgresWithoutDatabase,
    /// A PostgreSQL URL but no lock directory to put the lock file in.
    NoLockDirectory,
    /// The lock file could not be created, opened, locked or written.
    Io {
        /// The lock file, or the database path being resolved.
        path: PathBuf,
        /// The error of the operating system.
        source: io::Error,
    },
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StartError::AlreadyRunning { lock, holder } => {
                write!(
                    f,
                    "another server process already serves this app database: the start lock {} is held by ",
                    lock.display()
                )?;
                match holder {
                    Some(pid) => write!(f, "process {pid}")?,
                    None => f.write_str("another process")?,
                }
                f.write_str(
                    "; an app database is served by exactly one server process, stop the other \
                     process first (docs/sync.md)",
                )
            }
            StartError::UnsupportedUrl => f.write_str(
                "database url has an unknown scheme, expected memory:, sqlite: or postgres://",
            ),
            StartError::InvalidSqlitePath => f.write_str("sqlite url does not name a file"),
            StartError::PostgresWithoutDatabase => f.write_str(
                "postgres url does not name a database; write the database name in the url",
            ),
            StartError::NoLockDirectory => {
                f.write_str("a postgres database needs a directory for the start lock")
            }
            StartError::Io { path, source } => {
                write!(f, "start lock {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for StartError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StartError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// The start lock of the app database a server process serves. It is released when the
/// value is dropped or the process ends; a server keeps it for its whole life.
#[derive(Debug)]
#[must_use = "the start lock is released when the guard is dropped"]
pub struct StartGuard {
    file: File,
    path: PathBuf,
}

impl StartGuard {
    /// Takes the start lock for the database at `url`, the URL the server opens it with.
    ///
    /// `postgres_lock_dir` is the directory for the lock file of a PostgreSQL database; it
    /// must exist and be on a local file system shared by every process of the app. It is
    /// not used for other databases. Returns `Ok(None)` for a database that lives inside the
    /// process (`memory:`, `sqlite::memory:`), which needs no guard.
    pub fn for_url(
        url: &str,
        postgres_lock_dir: Option<&Path>,
    ) -> Result<Option<StartGuard>, StartError> {
        match lock_path(url, postgres_lock_dir)? {
            Some(path) => StartGuard::acquire(&path).map(Some),
            None => Ok(None),
        }
    }

    /// Takes the exclusive lock on the lock file at `path`, creating the file if needed.
    pub fn acquire(path: &Path) -> Result<StartGuard, StartError> {
        let io = |source| StartError::Io {
            path: path.to_path_buf(),
            source,
        };
        for _ in 0..MAX_ATTEMPTS {
            // Never truncate on open: the file may be held by another process, and its
            // contents name that process.
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(path)
                .map_err(io)?;
            match file.try_lock() {
                Ok(()) => {}
                Err(TryLockError::WouldBlock) => {
                    return Err(StartError::AlreadyRunning {
                        lock: path.to_path_buf(),
                        holder: read_holder(&file),
                    });
                }
                Err(TryLockError::Error(source)) => return Err(io(source)),
            }
            // The path may have been replaced or removed between open and lock; then this
            // lock guards a file no other process will open, and the new file is tried.
            if !still_at_path(&file, path).map_err(io)? {
                continue;
            }
            file.set_len(0).map_err(io)?;
            (&file)
                .write_all(format!("{}\n", std::process::id()).as_bytes())
                .map_err(io)?;
            return Ok(StartGuard {
                file,
                path: path.to_path_buf(),
            });
        }
        Err(io(io::Error::other(
            "the lock file was replaced while it was being locked",
        )))
    }

    /// The lock file this guard holds.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for StartGuard {
    fn drop(&mut self) {
        // Closing the file releases the lock as well; unlocking first makes the release
        // independent of other handles to the same open file. The file stays on disk.
        let _ = self.file.unlock();
    }
}

/// The lock file for the database at `url`, or `None` when the database needs no guard.
///
/// See the module documentation for where the file lies. `sqlite:` paths are resolved
/// against the current directory, as the SQLite adapter opens them.
pub fn lock_path(
    url: &str,
    postgres_lock_dir: Option<&Path>,
) -> Result<Option<PathBuf>, StartError> {
    if url.starts_with(MEMORY) {
        return Ok(None);
    }
    if let Some(path) = url.strip_prefix(SQLITE) {
        if path == SQLITE_MEMORY {
            return Ok(None);
        }
        if path.is_empty() {
            return Err(StartError::InvalidSqlitePath);
        }
        return sqlite_lock_path(Path::new(path)).map(Some);
    }
    if let Some(rest) = POSTGRES.iter().find_map(|scheme| url.strip_prefix(scheme)) {
        let name = postgres_database(rest)?;
        let dir = postgres_lock_dir.ok_or(StartError::NoLockDirectory)?;
        let digest: [u8; 32] = Sha256::digest(name.as_bytes()).into();
        return Ok(Some(dir.join(format!("ostrel-pg-{}.lock", hex(&digest)))));
    }
    Err(StartError::UnsupportedUrl)
}

/// `<file>.lock` in the resolved directory of the SQLite database file at `path`.
fn sqlite_lock_path(path: &Path) -> Result<PathBuf, StartError> {
    let io = |source| StartError::Io {
        path: path.to_path_buf(),
        source,
    };
    let mut file = path.to_path_buf();
    let mut links = 0;
    // Follow symbolic links by hand: the target may not exist yet (SQLite creates it), and
    // the lock must lie next to the file SQLite really opens.
    loop {
        match fs::symlink_metadata(&file) {
            Ok(meta) if meta.file_type().is_symlink() => {
                links += 1;
                if links > MAX_LINKS {
                    return Err(io(io::Error::other("too many levels of symbolic links")));
                }
                let target = fs::read_link(&file).map_err(io)?;
                file = match file.parent() {
                    Some(parent) if target.is_relative() => parent.join(target),
                    _ => target,
                };
            }
            Ok(_) => break,
            Err(e) if e.kind() == io::ErrorKind::NotFound => break,
            Err(e) => return Err(io(e)),
        }
    }
    let name = file.file_name().ok_or(StartError::InvalidSqlitePath)?;
    let parent = match file.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let dir = fs::canonicalize(parent).map_err(io)?;
    let mut lock = OsString::from(name);
    lock.push(LOCK_SUFFIX);
    Ok(dir.join(lock))
}

/// The database name of a PostgreSQL URL after its scheme: the `dbname` parameter if given,
/// otherwise the path. Percent escapes are decoded. A URL without a name is refused, since
/// the default (the user name) depends on settings the guard cannot see.
fn postgres_database(rest: &str) -> Result<String, StartError> {
    let rest = rest.split_once('#').map_or(rest, |(before, _)| before);
    let (location, query) = rest.split_once('?').unwrap_or((rest, ""));
    let path = location.split_once('/').map_or("", |(_, path)| path);
    let mut name = path;
    for pair in query.split('&') {
        if let Some(value) = pair.strip_prefix("dbname=") {
            name = value;
        }
    }
    let name = percent_decode(name).ok_or(StartError::PostgresWithoutDatabase)?;
    if name.is_empty() {
        return Err(StartError::PostgresWithoutDatabase);
    }
    Ok(name)
}

/// Decodes `%XX` escapes; `None` for a broken escape or a result that is not UTF-8.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b'%' {
            let hi = bytes.get(i + 1).and_then(|c| (*c as char).to_digit(16))?;
            let lo = bytes.get(i + 2).and_then(|c| (*c as char).to_digit(16))?;
            out.push(u8::try_from(hi * 16 + lo).ok()?);
            i += 3;
        } else {
            out.push(b);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The process id the holder wrote into the lock file, if it is there yet.
fn read_holder(file: &File) -> Option<u32> {
    let mut text = String::new();
    let mut reader = file.take(MAX_PID_BYTES);
    reader.read_to_string(&mut text).ok()?;
    text.trim().parse().ok()
}

/// Whether the open `file` is still the file at `path`.
#[cfg(unix)]
fn still_at_path(file: &File, path: &Path) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let open = file.metadata()?;
    match fs::metadata(path) {
        Ok(now) => Ok(open.dev() == now.dev() && open.ino() == now.ino()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Whether the open `file` is still the file at `path`. Without inode numbers only the
/// existence of the path can be checked.
#[cfg(not(unix))]
fn still_at_path(_file: &File, path: &Path) -> io::Result<bool> {
    match fs::metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Lowercase hex digits of `bytes`.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // Writing to a String cannot fail.
        let _ = write!(out, "{byte:02x}");
    }
    out
}
