//! The version format (Stage 4): a canonical Merkle encoding of a
//! path -> (content key, mode) map as ordinary core blobs.
//!
//! The core knows nothing of this. Every node blob starts with a shape tag,
//! so several tree shapes can coexist; [`SHAPES`] lists the ones this crate
//! reads and writes. Whatever the shape, the same entry set written with the
//! same algorithm gives the same root key. See `spec/version.md`.

pub mod fs;
pub mod path_tree;

use cas_core::algo::Algorithm;
use cas_core::key::{Parsed, classify};
use cas_core::{Expect, Store};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;

/// A file path: non-empty `/`-separated names, each a valid [`valid_name`].
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Mode {
    File,
    Exec,
    /// The content blob holds the link target.
    Link,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::File => "file",
            Mode::Exec => "exec",
            Mode::Link => "link",
        }
    }

    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "file" => Some(Mode::File),
            "exec" => Some(Mode::Exec),
            "link" => Some(Mode::Link),
            _ => None,
        }
    }
}

/// What a version maps a path to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Entry {
    key: String,
    pub mode: Mode,
}

impl Entry {
    /// `key` must be a syntactically valid blob key.
    pub fn new(key: impl Into<String>, mode: Mode) -> Result<Entry> {
        let key = key.into();
        if is_blob_key(&key) { Ok(Entry { key, mode }) } else { Err(Error::BadKey(key)) }
    }

    pub fn key(&self) -> &str {
        &self.key
    }
}

/// A blob key of a registered algorithm, with a digest of its length.
pub(crate) fn is_blob_key(key: &str) -> bool {
    match classify(key) {
        Some(Parsed::Blob { alg, digest_hex }) => {
            cas_core::algo::lookup(alg).is_some_and(|a| digest_hex.len() == a.digest_len * 2)
        }
        _ => false,
    }
}

/// One change to apply with [`update`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    /// Map the path to this entry, replacing whatever is there, including a
    /// whole directory.
    Set(Entry),
    /// Remove the path and everything under it. Absent paths are fine.
    Remove,
}

/// One difference between two versions. At least one side is present.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub path: Path,
    pub old: Option<Entry>,
    pub new: Option<Entry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Store(cas_core::Error),
    /// A blob the version refers to is absent from the store.
    Missing(String),
    /// A node that is not the canonical encoding of any tree.
    Malformed {
        key: String,
        reason: String,
    },
    UnknownShape(String),
    BadPath(String),
    BadKey(String),
    /// The edits would make this path both a file and a directory.
    PathConflict(Path),
    Io(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Store(e) => write!(f, "store: {e}"),
            Error::Missing(k) => write!(f, "missing blob {k}"),
            Error::Malformed { key, reason } => write!(f, "malformed node {key}: {reason}"),
            Error::UnknownShape(s) => write!(f, "unknown tree shape {s:?}"),
            Error::BadPath(p) => write!(f, "bad path {p:?}"),
            Error::BadKey(k) => write!(f, "not a blob key: {k:?}"),
            Error::PathConflict(p) => write!(f, "{p} would be both a file and a directory"),
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

/// Entries in canonical path order.
pub type Walk<'s> = Box<dyn Iterator<Item = Result<(Path, Entry)>> + 's>;

/// One tree shape. Implementations must be canonical: equal entry sets
/// written with the same algorithm give equal roots, whatever the order of
/// edits that produced them.
pub trait Shape: Sync {
    /// The tag in every node's header, such as `path/1`.
    fn name(&self) -> &'static str;

    /// Write a version holding exactly `entries`; returns its root key.
    fn build(&self, store: &dyn Store, alg: &'static Algorithm, entries: &BTreeMap<Path, Entry>) -> Result<String>;

    /// Apply `edits` to the version at `root`. Cost should scale with the
    /// edits, not the version.
    fn update(
        &self,
        store: &dyn Store,
        alg: &'static Algorithm,
        root: &str,
        edits: &BTreeMap<Path, Edit>,
    ) -> Result<String>;

    fn lookup(&self, store: &dyn Store, root: &str, path: &Path) -> Result<Option<Entry>>;

    /// Entries at or under `prefix` (all of them for `None`), in canonical
    /// order, reading only the nodes it needs.
    fn walk<'s>(&self, store: &'s dyn Store, root: &str, prefix: Option<&Path>) -> Walk<'s>;

    /// Differences at or under `prefix`, in canonical path order.
    fn diff(&self, store: &dyn Store, a: &str, b: &str, prefix: Option<&Path>) -> Result<Vec<Change>> {
        merge_diff(self.walk(store, a, prefix), self.walk(store, b, prefix))
    }
}

pub static SHAPES: &[&dyn Shape] = &[&path_tree::PathTree];

pub fn shape(name: &str) -> Result<&'static dyn Shape> {
    SHAPES.iter().copied().find(|s| s.name() == name).ok_or_else(|| Error::UnknownShape(name.to_string()))
}

/// Every node starts with this, then the shape name and a newline.
pub const MAGIC: &str = "cas-tree ";

/// The shape of the version at `root`, from its header.
pub fn shape_of(store: &dyn Store, root: &str) -> Result<&'static dyn Shape> {
    let bytes = read_blob(store, root)?;
    let name = bytes
        .iter()
        .position(|&b| b == b'\n')
        .and_then(|n| bytes[..n].strip_prefix(MAGIC.as_bytes()))
        .and_then(|n| std::str::from_utf8(n).ok())
        .ok_or_else(|| malformed(root, "no cas-tree header"))?;
    shape(name)
}

pub fn lookup(store: &dyn Store, root: &str, path: &Path) -> Result<Option<Entry>> {
    shape_of(store, root)?.lookup(store, root, path)
}

pub fn walk<'s>(store: &'s dyn Store, root: &str, prefix: Option<&Path>) -> Result<Walk<'s>> {
    Ok(shape_of(store, root)?.walk(store, root, prefix))
}

pub fn update(store: &dyn Store, alg: &'static Algorithm, root: &str, edits: &BTreeMap<Path, Edit>) -> Result<String> {
    shape_of(store, root)?.update(store, alg, root, edits)
}

/// Same shape: the shape's own diff, which skips equal subtrees. Different
/// shapes: compare both walks.
pub fn diff(store: &dyn Store, a: &str, b: &str, prefix: Option<&Path>) -> Result<Vec<Change>> {
    let (sa, sb) = (shape_of(store, a)?, shape_of(store, b)?);
    if sa.name() == sb.name() {
        sa.diff(store, a, b, prefix)
    } else {
        merge_diff(sa.walk(store, a, prefix), sb.walk(store, b, prefix))
    }
}

/// Diff two canonical-order walks by merging them.
pub fn merge_diff(a: Walk<'_>, b: Walk<'_>) -> Result<Vec<Change>> {
    let mut out = Vec::new();
    let (mut a, mut b) = (a.peekable(), b.peekable());
    loop {
        let order = match (a.peek(), b.peek()) {
            (None, None) => return Ok(out),
            (Some(Err(_)), _) => return Err(a.next().unwrap().unwrap_err()),
            (_, Some(Err(_))) => return Err(b.next().unwrap().unwrap_err()),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (Some(Ok((pa, _))), Some(Ok((pb, _)))) => pa.cmp(pb),
        };
        match order {
            Ordering::Less => {
                let (path, old) = a.next().unwrap()?;
                out.push(Change { path, old: Some(old), new: None });
            }
            Ordering::Greater => {
                let (path, new) = b.next().unwrap()?;
                out.push(Change { path, old: None, new: Some(new) });
            }
            Ordering::Equal => {
                let (path, old) = a.next().unwrap()?;
                let (_, new) = b.next().unwrap()?;
                if old != new {
                    out.push(Change { path, old: Some(old), new: Some(new) });
                }
            }
        }
    }
}

pub(crate) fn read_blob(store: &dyn Store, key: &str) -> Result<Vec<u8>> {
    if !is_blob_key(key) {
        return Err(Error::BadKey(key.to_string()));
    }
    store.read(key)?.ok_or_else(|| Error::Missing(key.to_string()))
}

/// Write `bytes` as a blob and return its key.
pub(crate) fn write_blob(store: &dyn Store, alg: &'static Algorithm, bytes: &[u8]) -> Result<String> {
    let key = alg.key(bytes);
    store.write(&key, bytes, &Expect::Absent)?;
    Ok(key)
}

pub(crate) fn malformed(key: &str, reason: impl Into<String>) -> Error {
    Error::Malformed { key: key.to_string(), reason: reason.into() }
}
