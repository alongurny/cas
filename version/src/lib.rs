//! The version format (Stage 4): a tree of directories and files stored as
//! ordinary core blobs. The core knows nothing of it.
//!
//! Blobs carry no headers. Whoever points at a blob also names how to read
//! it: a [`Ref`] is a pair (format, key), and a format's id is itself a blob
//! key, that of `format\n` followed by the format's spec text. Directory
//! formats hold references to their children, so formats nest. A version is
//! a reference to its root directory. See `spec/version.md`.

pub mod dir;
pub mod formats;
pub mod fs;
mod tree;

pub use formats::{Format, Kind, by_alias, by_id, publish};
pub use tree::{Walk, build, diff, lookup, merge_diff, update, walk};

use cas_core::algo::Algorithm;
use cas_core::key::{Parsed, classify};
use cas_core::{Expect, Store};
use std::cmp::Ordering;
use std::fmt;

/// How to read a blob: the id of its format, and its key.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ref {
    format: String,
    key: String,
}

impl Ref {
    /// Both parts must be valid blob keys.
    pub fn new(format: impl Into<String>, key: impl Into<String>) -> Result<Ref> {
        let (format, key) = (format.into(), key.into());
        for k in [&format, &key] {
            if !is_blob_key(k) {
                return Err(Error::BadKey(k.clone()));
            }
        }
        Ok(Ref { format, key })
    }

    /// Parse `<format> <key>`.
    pub fn parse(s: &str) -> Result<Ref> {
        let (f, k) = s.split_once(' ').ok_or_else(|| Error::BadKey(s.to_string()))?;
        Ref::new(f, k)
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

    pub(crate) fn is_dir(&self) -> bool {
        self.known_format().is_some_and(|f| matches!(f.kind, Kind::Dir(_)))
    }
}

impl fmt::Display for Ref {
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
    fn cmp(&self, other: &Path) -> Ordering {
        self.names().cmp(other.names())
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
/// paths, so `Dir` carries nothing; any reference whose format is not a
/// known directory format is a `File`, read with that format.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Node {
    Dir,
    File(Ref),
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
    Io(String),
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
            Error::Io(e) => write!(f, "io: {e}"),
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
    store.read(key)?.ok_or_else(|| Error::Missing(key.to_string()))
}

/// Write `bytes` as a blob and return its key.
pub fn write_blob(store: &dyn Store, alg: &'static Algorithm, bytes: &[u8]) -> Result<String> {
    let key = alg.key(bytes);
    store.write(&key, bytes, &Expect::Absent)?;
    Ok(key)
}

/// Write `bytes` and return a reference to them in format `alias`, which
/// must be a format this crate knows.
pub fn put(store: &dyn Store, alg: &'static Algorithm, alias: &str, bytes: &[u8]) -> Result<Ref> {
    let f = by_alias(alias).ok_or_else(|| Error::UnknownFormat(alias.to_string()))?;
    let key = alg.key(bytes);
    f.check(&key, bytes)?;
    store.write(&key, bytes, &Expect::Absent)?;
    Ok(Ref { format: f.id().to_string(), key })
}

pub(crate) fn malformed(key: &str, reason: impl Into<String>) -> Error {
    Error::Malformed { key: key.to_string(), reason: reason.into() }
}
