//! The version format (Stage 4): a tree of directories and files stored as
//! ordinary core blobs. The core knows nothing of it.
//!
//! Blobs carry no headers. Whoever points at a blob also names how to read
//! it: a [`View`] is a pair (format, key), and a format's id is itself a blob
//! key, that of `format\n` followed by the format's spec text. Directory
//! formats hold views of their children, so formats nest. A version is
//! a view of its root directory. See `spec/version.md`.

pub mod access;
pub mod ancestry;
pub mod annotated;
pub mod derived;
pub mod dir;
pub mod formats;
pub mod fs;
pub mod fsck;
pub mod gc;
pub mod git;
mod git_notes;
pub mod graph;
pub mod history;
mod htree;
pub mod index;
pub mod merge;
pub mod sync;
mod tree;
pub mod uni;
pub mod watch;

pub use formats::{Format, Kind, by_alias, by_id, publish};
pub use tree::{Walk, build, diff, lookup, merge_diff, object_at, update, walk};

use cas_core::algo::Algorithm;
use cas_core::key::{Parsed, classify};
use cas_core::{Expect, Store};
use std::cmp::Ordering;
use std::fmt;

/// How to read a blob: the id of its format, and its key.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct View {
    format: String,
    key: String,
}

impl View {
    /// Both parts must be valid blob keys.
    pub fn new(format: impl Into<String>, key: impl Into<String>) -> Result<View> {
        let (format, key) = (format.into(), key.into());
        for k in [&format, &key] {
            if !is_blob_key(k) {
                return Err(Error::BadKey(k.clone()));
            }
        }
        Ok(View { format, key })
    }

    /// Parse `<format> <key>`.
    pub fn parse(s: &str) -> Result<View> {
        let (f, k) = s.split_once(' ').ok_or_else(|| Error::BadKey(s.to_string()))?;
        View::new(f, k)
    }

    pub fn format(&self) -> &str {
        &self.format
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    /// The format, when this crate knows it.
    pub fn known_format(&self) -> Option<&'static Format> {
        by_id(&self.format)
    }

    /// Whether this is a view of a directory.
    pub fn is_dir_view(&self) -> bool {
        self.is_dir()
    }

    pub(crate) fn is_dir(&self) -> bool {
        self.known_format().is_some_and(|f| matches!(f.kind, Kind::Dir(_)))
    }
}

impl fmt::Display for View {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.format, self.key)
    }
}

/// A file path: non-empty `/`-separated names, each a [`valid_name`].
/// Ordered name by name (canonical order), so a directory's contents sort
/// right after it and before its next sibling.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Path(String);

/// One path component: non-empty, not `.` or `..`, no `/`, no control
/// characters.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.chars().any(char::is_control)
}

impl Path {
    pub fn new(s: impl Into<String>) -> Result<Path> {
        let s = s.into();
        if s.split('/').all(valid_name) { Ok(Path(s)) } else { Err(Error::BadPath(s)) }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn names(&self) -> std::str::Split<'_, char> {
        self.0.split('/')
    }

    /// True when `self` is `prefix` or lies under it.
    pub fn starts_with(&self, prefix: &Path) -> bool {
        self.0 == prefix.0 || (self.0.starts_with(&prefix.0) && self.0.as_bytes()[prefix.0.len()] == b'/')
    }

    pub fn parent(&self) -> Option<Path> {
        self.0.rsplit_once('/').map(|(p, _)| Path(p.to_string()))
    }

    /// `dir/name`, or just `name` at the root. `name` must be valid.
    pub(crate) fn child(dir: Option<&Path>, name: &str) -> Path {
        match dir {
            Some(d) => Path(format!("{}/{name}", d.0)),
            None => Path(name.to_string()),
        }
    }
}

impl Ord for Path {
    /// Name by name. Names hold no `/` and no control byte, so comparing
    /// the bytes with `/` ranked below every other byte gives the same order
    /// without splitting the paths.
    fn cmp(&self, other: &Path) -> Ordering {
        let (a, b) = (self.as_str().as_bytes(), other.as_str().as_bytes());
        let rank = |c: u8| if c == b'/' { 0 } else { c };
        match a.iter().zip(b).position(|(x, y)| x != y) {
            Some(i) => rank(a[i]).cmp(&rank(b[i])),
            None => a.len().cmp(&b.len()),
        }
    }
}

impl PartialOrd for Path {
    fn partial_cmp(&self, other: &Path) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a version holds at a path. A directory's contents are separate
/// paths, so `Dir` carries nothing; any view whose format is not a
/// known directory format is a `File`, read with that format.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Node {
    Dir,
    File(View),
}

/// One change to apply with [`update`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    /// `Set(Dir)` keeps a directory that is already there and otherwise
    /// replaces the path with an empty one. `Set(File)` replaces whatever
    /// is at the path, including a whole directory. Missing parent
    /// directories are created.
    Set(Node),
    /// Remove the path and everything under it. Absent paths are fine.
    Remove,
}

/// One difference between two versions. At least one side is present.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub path: Path,
    pub old: Option<Node>,
    pub new: Option<Node>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Store(cas_core::Error),
    /// A blob the version refers to is absent from the store.
    Missing(String),
    /// A blob that is not valid in the format it was read with.
    Malformed {
        key: String,
        reason: String,
    },
    /// A format this crate cannot read, where it has to.
    UnknownFormat(String),
    BadPath(String),
    BadKey(String),
    /// The edits would put something under a file.
    PathConflict(Path),
    /// A branch moved in a way that would lose history if overwritten.
    Diverged(String),
    Io(String),
    /// An access policy or grant that does not parse (spec/access.md).
    Policy(String),
    /// Notes where a release history allows none (spec/release-history.md).
    NotRelease(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Store(e) => write!(f, "store: {e}"),
            Error::Missing(k) => write!(f, "missing blob {k}"),
            Error::Malformed { key, reason } => write!(f, "malformed blob {key}: {reason}"),
            Error::UnknownFormat(s) => write!(f, "unknown format {s}"),
            Error::BadPath(p) => write!(f, "bad path {p:?}"),
            Error::BadKey(k) => write!(f, "not a blob key: {k:?}"),
            Error::PathConflict(p) => write!(f, "{p} is a file, so nothing can go under it"),
            Error::Diverged(t) => write!(f, "branch {t} has diverged"),
            Error::Io(e) => write!(f, "io: {e}"),
            Error::Policy(e) => write!(f, "policy: {e}"),
            Error::NotRelease(e) => write!(f, "not a release history: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<cas_core::Error> for Error {
    fn from(e: cas_core::Error) -> Error {
        Error::Store(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error::Io(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// A blob key of a registered algorithm, with a digest of its length.
pub(crate) fn is_blob_key(key: &str) -> bool {
    match classify(key) {
        Some(Parsed::Blob { alg, digest_hex }) => {
            cas_core::algo::lookup(alg).is_some_and(|a| digest_hex.len() == a.digest_len * 2)
        }
        _ => false,
    }
}

pub fn read_blob(store: &dyn Store, key: &str) -> Result<Vec<u8>> {
    if !is_blob_key(key) {
        return Err(Error::BadKey(key.to_string()));
    }
    match store.read(key)? {
        Some(v) => Ok(v),
        None => Err(Error::Missing(key.to_string())),
    }
}

/// Write `bytes` as a blob and return its key.
pub fn write_blob(store: &dyn Store, alg: &'static Algorithm, bytes: &[u8]) -> Result<String> {
    let key = alg.key(bytes);
    store.write(&key, bytes, &Expect::Absent)?;
    Ok(key)
}

/// Write `bytes` and return a view of them in format `alias`, which
/// must be a format this crate knows.
pub fn put(store: &dyn Store, alg: &'static Algorithm, alias: &str, bytes: &[u8]) -> Result<View> {
    let f = by_alias(alias).ok_or_else(|| Error::UnknownFormat(alias.to_string()))?;
    let key = alg.key(bytes);
    f.check(&key, bytes)?;
    store.write(&key, bytes, &Expect::Absent)?;
    Ok(View { format: f.id().to_string(), key })
}

pub(crate) fn malformed(key: &str, reason: impl Into<String>) -> Error {
    Error::Malformed { key: key.to_string(), reason: reason.into() }
}

#[cfg(test)]
mod path_order {
    use super::Path;

    #[test]
    fn byte_order_with_slash_lowest_is_name_order() {
        let names = ["a", "a-b", "a.b", "a b", "ab", "b", "\u{e9}", "a~", "A", "0"];
        let mut paths = Vec::new();
        for x in names {
            paths.push(x.to_string());
            for y in names {
                paths.push(format!("{x}/{y}"));
                paths.push(format!("{x}/{y}/{x}"));
            }
        }
        for a in &paths {
            for b in &paths {
                let (p, q) = (Path::new(a.clone()).unwrap(), Path::new(b.clone()).unwrap());
                assert_eq!(p.cmp(&q), p.names().cmp(q.names()), "{a} vs {b}");
            }
        }
    }
}
