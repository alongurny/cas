//! The status watcher (git's fsmonitor): `cas watch DIR` follows a working
//! directory with inotify and logs every path that changes, so that a
//! commit or status reads the stat cache for everything else instead of
//! looking at every file again (spec/watch.md).
//!
//! The watcher holds an exclusive lock on [`LOCK`] for as long as it runs,
//! and writes [`LOG`]: a header naming this run, then one changed path per
//! line, relative to the directory. A line `!` means it may have missed
//! events (the kernel's queue overflowed, or a name it cannot log): the
//! next reader looks at everything. A reader takes a token, (run, log
//! length), before it looks at the directory, and the stat cache keeps the
//! token it was written after: next time, only the paths logged since then
//! need a look. Without a running watcher, or with a token of another run,
//! everything is looked at, as without one.

use std::collections::{BTreeSet, HashMap};
use std::ffi::CString;
use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path as FsPath, PathBuf};

/// The log inside the working directory.
pub const LOG: &str = ".cas-watch";
/// The lock the running watcher holds.
pub const LOCK: &str = ".cas-watch.lock";
const MAGIC: &str = "CASWATCH1 ";

/// Whether a top-level name is the tool's own (cache, log, lock, sparse
/// list): never imported and never logged.
pub fn own_name(name: &str) -> bool {
    name.starts_with(crate::index::FILE) || name.starts_with(LOG) || name == crate::fs::SPARSE
}

/// Where a reader stands in a watcher's log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    /// The watcher run.
    pub run: String,
    /// The log's length when the token was taken.
    pub at: u64,
}

/// The token of the watcher running on `dir`, if one is.
pub fn token(dir: &FsPath) -> Option<Token> {
    if !running(dir) {
        return None;
    }
    let mut f = fs::File::open(dir.join(LOG)).ok()?;
    let at = f.metadata().ok()?.len();
    let mut head = [0u8; 64];
    let n = f.read(&mut head).ok()?;
    let line = std::str::from_utf8(&head[..n]).ok()?.split('\n').next()?;
    let run = line.strip_prefix(MAGIC)?.to_string();
    // The header is written last when the watcher starts: without its
    // newline the log is not ready.
    (n > line.len()).then_some(Token { run, at })
}

/// Whether a watcher holds the lock on `dir`.
fn running(dir: &FsPath) -> bool {
    let Ok(f) = fs::File::open(dir.join(LOCK)) else { return false };
    // SAFETY: flock on a file descriptor we own.
    let r = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) };
    if r == 0 {
        // SAFETY: as above.
        unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_UN) };
        return false;
    }
    io::Error::last_os_error().raw_os_error() == Some(libc::EWOULDBLOCK)
}

/// The paths logged between `since` and `now` (two tokens of one run), or
/// `None` when the log cannot tell (another run, or events were missed).
pub fn changed(dir: &FsPath, since: &Token, now: &Token) -> Option<BTreeSet<String>> {
    if since.run != now.run || since.at > now.at {
        return None;
    }
    let mut f = fs::File::open(dir.join(LOG)).ok()?;
    let mut b = Vec::new();
    io::Seek::seek(&mut f, io::SeekFrom::Start(since.at)).ok()?;
    f.take(now.at - since.at).read_to_end(&mut b).ok()?;
    let mut out = BTreeSet::new();
    for line in b.split(|&c| c == b'\n') {
        match line {
            b"" => {}
            b"!" => return None,
            p => {
                out.insert(String::from_utf8(p.to_vec()).ok()?);
            }
        }
    }
    Some(out)
}

/// Whether `path` or a directory above it is in `changed`.
pub fn touched(path: &str, changed: &BTreeSet<String>) -> bool {
    if changed.is_empty() {
        return false;
    }
    let mut end = path.len();
    loop {
        if changed.contains(&path[..end]) {
            return true;
        }
        match path[..end].rfind('/') {
            Some(i) => end = i,
            None => return false,
        }
    }
}

const MASK: u32 = libc::IN_MODIFY
    | libc::IN_ATTRIB
    | libc::IN_CLOSE_WRITE
    | libc::IN_MOVED_FROM
    | libc::IN_MOVED_TO
    | libc::IN_CREATE
    | libc::IN_DELETE
    | libc::IN_DELETE_SELF
    | libc::IN_MOVE_SELF
    | libc::IN_ONLYDIR;

/// A running watcher: inotify watches on every directory under the root.
pub struct Watcher {
    root: PathBuf,
    fd: OwnedFd,
    /// Watch descriptor to the directory's path ("" for the root).
    dirs: HashMap<i32, String>,
    log: fs::File,
    _lock: fs::File,
}

impl Watcher {
    /// Start watching `root`: lock, watch every directory, then write the
    /// log's header (from then on readers may use it). Fails if another
    /// watcher runs there.
    pub fn start(root: &FsPath) -> io::Result<Watcher> {
        let lock = fs::OpenOptions::new().create(true).truncate(false).write(true).open(root.join(LOCK))?;
        // SAFETY: flock on a descriptor we own.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::other(format!("a watcher already runs on {}", root.display())));
        }
        // SAFETY: a plain syscall; the descriptor is owned below.
        let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fd is a fresh descriptor nobody else owns.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let log = fs::OpenOptions::new().create(true).write(true).truncate(true).open(root.join(LOG))?;
        let mut w = Watcher { root: root.to_path_buf(), fd, dirs: HashMap::new(), log, _lock: lock };
        let mut lost = Vec::new();
        w.add_tree("", &mut lost)?;
        let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        let run = format!("{}.{}.{}", std::process::id(), d.as_secs(), d.subsec_nanos());
        w.log.write_all(format!("{MAGIC}{run}\n").as_bytes())?;
        Ok(w)
    }

    /// Watch the directory `rel` and everything under it, appending to `out`
    /// the paths found (they may have appeared before the watch).
    fn add_tree(&mut self, rel: &str, out: &mut Vec<String>) -> io::Result<()> {
        let full = if rel.is_empty() { self.root.clone() } else { self.root.join(rel) };
        let c = CString::new(full.as_os_str().as_bytes()).map_err(io::Error::other)?;
        // SAFETY: c is a valid C string; fd is our inotify descriptor.
        let wd = unsafe { libc::inotify_add_watch(self.fd.as_raw_fd(), c.as_ptr(), MASK) };
        if wd < 0 {
            // Gone already (removed while we looked): its parent logged it.
            return match io::Error::last_os_error().raw_os_error() {
                Some(libc::ENOENT | libc::ENOTDIR) => Ok(()),
                _ => Err(io::Error::last_os_error()),
            };
        }
        self.dirs.insert(wd, rel.to_string());
        let Ok(items) = fs::read_dir(&full) else { return Ok(()) };
        for item in items.flatten() {
            let name = item.file_name().to_string_lossy().into_owned();
            if rel.is_empty() && own_name(&name) {
                continue;
            }
            let path = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            out.push(path.clone());
            if item.file_type().is_ok_and(|t| t.is_dir()) {
                self.add_tree(&path, out)?;
            }
        }
        Ok(())
    }

    /// Log changes until an error ends the watch (the root going away).
    pub fn run(mut self) -> io::Result<()> {
        let mut buf = vec![0u8; 64 << 10];
        loop {
            // SAFETY: buf is valid for its length; fd is our descriptor.
            let n = unsafe { libc::read(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            let mut lines = Vec::new();
            let mut new_dirs = Vec::new();
            let mut at = 0;
            let head = std::mem::size_of::<libc::inotify_event>();
            while at + head <= n as usize {
                // SAFETY: the kernel wrote a whole event header here; read
                // unaligned since the buffer is plain bytes.
                let ev: libc::inotify_event = unsafe { std::ptr::read_unaligned(buf[at..].as_ptr().cast()) };
                let name_bytes = &buf[at + head..at + head + ev.len as usize];
                at += head + ev.len as usize;
                if ev.mask & libc::IN_Q_OVERFLOW != 0 {
                    lines.push("!".to_string());
                    continue;
                }
                let Some(dir) = self.dirs.get(&ev.wd).cloned() else { continue };
                if ev.mask & libc::IN_IGNORED != 0 {
                    self.dirs.remove(&ev.wd);
                    continue;
                }
                let name = name_bytes.split(|&c| c == 0).next().unwrap_or_default();
                let path = if name.is_empty() {
                    dir.clone()
                } else {
                    let Ok(name) = std::str::from_utf8(name) else {
                        lines.push("!".to_string());
                        continue;
                    };
                    if dir.is_empty() && own_name(name) {
                        continue;
                    }
                    if dir.is_empty() { name.to_string() } else { format!("{dir}/{name}") }
                };
                if path.contains('\n') || path.is_empty() && ev.mask & (libc::IN_DELETE_SELF | libc::IN_MOVE_SELF) != 0
                {
                    // The root itself moved or went: nothing to follow.
                    lines.push("!".to_string());
                    self.flush(&lines)?;
                    return Ok(());
                }
                if ev.mask & libc::IN_ISDIR != 0 && ev.mask & (libc::IN_CREATE | libc::IN_MOVED_TO) != 0 {
                    new_dirs.push(path.clone());
                }
                lines.push(path);
            }
            for d in new_dirs {
                self.add_tree(&d, &mut lines)?;
            }
            self.flush(&lines)?;
        }
    }

    fn flush(&mut self, lines: &[String]) -> io::Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        let mut b = String::new();
        for l in lines {
            b.push_str(l);
            b.push('\n');
        }
        self.log.write_all(b.as_bytes())
    }
}
