//! Start guard: a second server process on the same app database does not start (D153).
//!
//! The two process cases run this test binary a second time as a child process. The child
//! runs only the test `child_process_entry`, which does its part when the environment
//! variable `OSTREL_START_GUARD_CHILD` is set and nothing otherwise.

#[path = "../src/single_process.rs"]
mod single_process;

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use single_process::{StartError, StartGuard, lock_path};

/// Child role: `hold` takes the lock and keeps it until stdin closes; `probe` tries once.
const CHILD_ROLE: &str = "OSTREL_START_GUARD_CHILD";
/// The database URL the child guards.
const CHILD_URL: &str = "OSTREL_START_GUARD_URL";
/// Marks the result line of the child in its output.
const MARK: &str = "start guard child: ";

/// A fresh, empty directory for the test `name`.
fn fresh_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("start_guard_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}

fn sqlite_url(path: &Path) -> String {
    format!("sqlite:{}", path.display())
}

fn guard(url: &str) -> Result<Option<StartGuard>, StartError> {
    StartGuard::for_url(url, None)
}

/// A child process that is killed when the test ends, also when it fails.
struct Kid(Child);

impl Drop for Kid {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Runs this test binary as a child in `role` for `url`, with `cwd` as working directory.
fn spawn_child(role: &str, url: &str, cwd: &Path) -> Kid {
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "child_process_entry",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ROLE, role)
        .env(CHILD_URL, url)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    Kid(child)
}

/// The first line of the child's output that starts with one of `prefixes`.
fn child_line(kid: &mut Kid, prefixes: &[&str]) -> String {
    let child = &mut kid.0;
    let stdout = child.stdout.as_mut().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).unwrap();
        if n == 0 {
            let mut err = String::new();
            let _ = child.stderr.as_mut().unwrap().read_to_string(&mut err);
            panic!("child ended without a result line; stderr: {err}");
        }
        // libtest prints `test child_process_entry ... ` on the same line before the result.
        if let Some(at) = line.find(MARK) {
            let result = line[at + MARK.len()..].trim_end();
            if prefixes.iter().any(|p| result.starts_with(p)) {
                return result.to_string();
            }
        }
    }
}

/// The child side of the two process tests; a no op in a normal test run.
#[test]
fn child_process_entry() {
    let Ok(role) = std::env::var(CHILD_ROLE) else {
        return;
    };
    let url = std::env::var(CHILD_URL).unwrap();
    match (role.as_str(), StartGuard::for_url(&url, None)) {
        ("hold", Ok(Some(held))) => {
            println!("{MARK}held {}", std::process::id());
            // Keep the lock until the parent closes stdin or kills this process.
            let mut rest = Vec::new();
            let _ = std::io::stdin().read_to_end(&mut rest);
            drop(held);
        }
        ("probe", Ok(Some(_))) => println!("{MARK}acquired"),
        (_, Err(e)) => println!("{MARK}refused {e}"),
        (other, Ok(_)) => panic!("unexpected result for {url} in role {other}"),
    }
}

#[test]
fn second_process_is_refused_and_named() {
    let dir = fresh_dir("two_processes");
    let url = sqlite_url(&dir.join("app.db"));
    let mut child = spawn_child("hold", &url, &dir);
    let line = child_line(&mut child, &["held ", "refused "]);
    assert_eq!(line, format!("held {}", child.0.id()));

    let err = guard(&url).unwrap_err();
    match &err {
        StartError::AlreadyRunning { lock, holder } => {
            assert_eq!(lock, &dir.join("app.db.lock"));
            assert_eq!(*holder, Some(child.0.id()));
        }
        other => panic!("expected AlreadyRunning, got {other:?}"),
    }
    let message = err.to_string();
    assert!(message.contains("another server process already serves this app database"));
    assert!(message.contains(&format!("process {}", child.0.id())));
    assert!(message.contains(&dir.join("app.db.lock").display().to_string()));

    // The holder ends normally: the lock is free again.
    drop(child.0.stdin.take());
    assert!(child.0.wait().unwrap().success());
    let held = guard(&url).unwrap().unwrap();
    assert_eq!(held.path(), dir.join("app.db.lock"));
}

#[test]
fn killed_holder_leaves_no_stale_lock() {
    let dir = fresh_dir("killed_holder");
    let url = sqlite_url(&dir.join("app.db"));
    let mut child = spawn_child("hold", &url, &dir);
    child_line(&mut child, &["held "]);
    assert!(matches!(
        guard(&url),
        Err(StartError::AlreadyRunning { .. })
    ));

    child.0.kill().unwrap();
    let _ = child.0.wait().unwrap();
    // The lock file stays on disk, but nobody holds it.
    assert!(dir.join("app.db.lock").exists());
    let held = guard(&url).unwrap().unwrap();
    // The new holder replaced the old process id with its own.
    let text = std::fs::read_to_string(held.path()).unwrap();
    assert_eq!(text, format!("{}\n", std::process::id()));
}

#[test]
fn child_with_relative_path_is_refused_while_parent_holds() {
    let dir = fresh_dir("relative");
    let held = guard(&sqlite_url(&dir.join("app.db"))).unwrap().unwrap();
    // The same file through a relative path in another working directory.
    let mut child = spawn_child("probe", "sqlite:app.db", &dir);
    let line = child_line(&mut child, &["refused ", "acquired"]);
    assert!(line.starts_with("refused another server process"), "{line}");
    assert!(
        line.contains(&format!("process {}", std::process::id())),
        "{line}"
    );
    assert!(child.0.wait().unwrap().success());
    drop(held);

    let mut child = spawn_child("probe", "sqlite:app.db", &dir);
    assert_eq!(
        child_line(&mut child, &["refused ", "acquired"]),
        "acquired"
    );
    assert!(child.0.wait().unwrap().success());
}

#[test]
fn second_guard_in_one_process_is_refused_until_the_first_drops() {
    let dir = fresh_dir("one_process");
    let url = sqlite_url(&dir.join("app.db"));
    let first = guard(&url).unwrap().unwrap();
    for _ in 0..2 {
        match guard(&url) {
            Err(StartError::AlreadyRunning { holder, .. }) => {
                assert_eq!(holder, Some(std::process::id()));
            }
            other => panic!("expected AlreadyRunning, got {other:?}"),
        }
    }
    // A refused attempt must not wipe the holder's process id.
    let text = std::fs::read_to_string(first.path()).unwrap();
    assert_eq!(text, format!("{}\n", std::process::id()));
    drop(first);
    assert!(guard(&url).unwrap().is_some());
}

#[test]
fn holder_without_process_id_is_reported_as_unknown() {
    let dir = fresh_dir("no_pid");
    let lock = dir.join("app.db.lock");
    let held = StartGuard::acquire(&lock).unwrap();
    // A holder that has not written its id yet, or wrote something else.
    std::fs::write(&lock, "not a pid\n").unwrap();
    let err = StartGuard::acquire(&lock).unwrap_err();
    assert!(matches!(
        err,
        StartError::AlreadyRunning { holder: None, .. }
    ));
    assert!(err.to_string().contains("held by another process"));
    drop(held);
}

#[test]
fn guard_is_taken_before_the_database_file_exists() {
    let dir = fresh_dir("no_file");
    let held = guard(&sqlite_url(&dir.join("new.db"))).unwrap().unwrap();
    assert!(!dir.join("new.db").exists());
    assert_eq!(held.path(), dir.join("new.db.lock"));
}

#[test]
fn spellings_of_one_sqlite_file_share_one_lock() {
    let dir = fresh_dir("spellings");
    std::fs::create_dir(dir.join("sub")).unwrap();
    let plain = dir.join("app.db");
    let want = Some(dir.join("app.db.lock"));
    let dotted = format!("{}/sub/../././app.db", dir.display());
    assert_eq!(lock_path(&sqlite_url(&plain), None).unwrap(), want);
    assert_eq!(lock_path(&format!("sqlite:{dotted}"), None).unwrap(), want);
}

#[cfg(unix)]
#[test]
fn symbolic_links_resolve_to_the_real_file_also_before_it_exists() {
    let dir = fresh_dir("links");
    std::fs::create_dir(dir.join("data")).unwrap();
    let want = Some(dir.join("data").join("app.db.lock"));
    // A link whose target does not exist yet (SQLite would create it).
    std::os::unix::fs::symlink("data/app.db", dir.join("link.db")).unwrap();
    assert_eq!(
        lock_path(&sqlite_url(&dir.join("link.db")), None).unwrap(),
        want
    );
    // A chain of links, and a linked directory.
    std::os::unix::fs::symlink(dir.join("link.db"), dir.join("chain.db")).unwrap();
    std::os::unix::fs::symlink("data", dir.join("datalink")).unwrap();
    std::fs::write(dir.join("data").join("app.db"), b"").unwrap();
    assert_eq!(
        lock_path(&sqlite_url(&dir.join("chain.db")), None).unwrap(),
        want
    );
    let via_dir = dir.join("datalink").join("app.db");
    assert_eq!(lock_path(&sqlite_url(&via_dir), None).unwrap(), want);
    // A loop of links is an error, not a hang.
    std::os::unix::fs::symlink("loop_b", dir.join("loop_a")).unwrap();
    std::os::unix::fs::symlink("loop_a", dir.join("loop_b")).unwrap();
    assert!(matches!(
        lock_path(&sqlite_url(&dir.join("loop_a")), None),
        Err(StartError::Io { .. })
    ));
}

#[test]
fn databases_inside_the_process_need_no_guard() {
    for url in ["memory:", "memory:anything", "sqlite::memory:"] {
        assert!(guard(url).unwrap().is_none(), "{url}");
        assert_eq!(lock_path(url, None).unwrap(), None, "{url}");
    }
}

#[test]
fn bad_urls_are_refused() {
    for url in ["", "MEMORY:", "mysql://h/db", "sqlite", "file:a.db"] {
        assert!(
            matches!(guard(url), Err(StartError::UnsupportedUrl)),
            "{url}"
        );
    }
    assert!(matches!(
        guard("sqlite:"),
        Err(StartError::InvalidSqlitePath)
    ));
    let dir = fresh_dir("bad_paths");
    let dotdot = format!("sqlite:{}/..", dir.display());
    assert!(matches!(guard(&dotdot), Err(StartError::InvalidSqlitePath)));
    // A directory that does not exist: SQLite could not open the file either.
    let missing = sqlite_url(&dir.join("missing").join("app.db"));
    let err = guard(&missing).unwrap_err();
    assert!(matches!(err, StartError::Io { .. }));
    assert!(std::error::Error::source(&err).is_some());
    assert!(err.to_string().starts_with("start lock "));
}

#[test]
fn postgres_lock_is_keyed_by_database_name_only() {
    let dir = fresh_dir("postgres");
    let lock = |url: &str| lock_path(url, Some(&dir)).unwrap().unwrap();
    let base = lock("postgres://localhost/issues");
    for same in [
        "postgresql://127.0.0.1:5432/issues",
        "postgres://app:secret@db.internal:6543/issues?sslmode=require",
        "postgres:///issues",
        "postgres://h/other?dbname=issues",
        "postgres://h/iss%75es",
        "postgres://h/issues#fragment",
    ] {
        assert_eq!(lock(same), base, "{same}");
    }
    assert_ne!(lock("postgres://localhost/issues2"), base);
    assert_ne!(lock("postgres://localhost/Issues"), base);
    let name = base.file_name().unwrap().to_str().unwrap().to_string();
    assert!(
        name.starts_with("ostrel-pg-") && name.ends_with(".lock"),
        "{name}"
    );
    assert_eq!(name.len(), "ostrel-pg-".len() + 64 + ".lock".len());
    assert_eq!(base.parent(), Some(dir.as_path()));

    // Two processes of one host on the same database name exclude each other.
    let url = "postgres://localhost/issues";
    let held = StartGuard::for_url(url, Some(&dir)).unwrap().unwrap();
    let other = StartGuard::for_url("postgresql://127.0.0.1/issues", Some(&dir));
    assert!(matches!(other, Err(StartError::AlreadyRunning { .. })));
    drop(held);
    assert!(StartGuard::for_url(url, Some(&dir)).unwrap().is_some());
}

#[test]
fn postgres_without_name_or_lock_directory_is_refused() {
    let dir = fresh_dir("postgres_bad");
    for url in [
        "postgres://localhost",
        "postgres://localhost/",
        "postgres://localhost/?sslmode=disable",
        "postgres://h/db?dbname=",
        "postgres://h/%zz",
        "postgres://h/%4",
        "postgres://h/%ff",
    ] {
        let err = lock_path(url, Some(&dir)).unwrap_err();
        assert!(matches!(err, StartError::PostgresWithoutDatabase), "{url}");
        assert!(err.to_string().contains("does not name a database"));
    }
    let err = lock_path("postgres://localhost/issues", None).unwrap_err();
    assert!(matches!(err, StartError::NoLockDirectory));
    assert!(err.to_string().contains("directory for the start lock"));
}
