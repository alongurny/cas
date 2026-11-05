//! The formats this crate reads and writes, identified by the keys of their
//! format objects (spec/metaformat.md). Aliases such as `dir` are local
//! names only; blobs and references always carry the id.

use crate::dir::{self, Dir};
use crate::{Error, Result, malformed};
use cas_core::algo::{self, Algorithm};
use cas_core::{Expect, Store};
use std::sync::LazyLock;

/// The metaformat document. Its blake3 key is the metaformat's id.
pub const METAFORMAT: &str = include_str!("../../spec/metaformat.md");

/// How this crate treats blobs of a format.
pub enum Kind {
    /// A directory: named references to other blobs.
    Dir(&'static Dir),
    /// A file, with a check that its bytes are valid.
    Leaf(fn(&[u8]) -> std::result::Result<(), String>),
}

pub struct Format {
    pub alias: &'static str,
    /// The spec document (markdown).
    pub spec: &'static str,
    pub kind: Kind,
    object: String,
    id: String,
}

impl Format {
    /// The key of the format object.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The format object, in the metaformat.
    pub fn object(&self) -> &str {
        &self.object
    }

    /// Whether `bytes`, stored at `key`, are valid in this format.
    pub fn check(&self, key: &str, bytes: &[u8]) -> Result<()> {
        match &self.kind {
            Kind::Dir(d) => d.decode(key, bytes).map(|_| ()),
            Kind::Leaf(check) => check(bytes).map_err(|r| malformed(key, r)),
        }
    }
}

fn blake3() -> &'static Algorithm {
    algo::lookup("blake3").unwrap()
}

fn format(alias: &'static str, spec: &'static str, kind: Kind) -> Format {
    let object = format!("name {alias}\nmedia text/markdown\nspec {}\n", blake3().key(spec.as_bytes()));
    let id = blake3().key(object.as_bytes());
    Format { alias, spec, kind, object, id }
}

fn any(_: &[u8]) -> std::result::Result<(), String> {
    Ok(())
}

fn symlink(b: &[u8]) -> std::result::Result<(), String> {
    if b.is_empty() || b.contains(&0) { Err("a link target is non-empty and has no NUL".into()) } else { Ok(()) }
}

/// `blob <n>\0<contents>`, the git loose object of a file.
pub fn git_blob(contents: &[u8]) -> Vec<u8> {
    let mut out = format!("blob {}\0", contents.len()).into_bytes();
    out.extend_from_slice(contents);
    out
}

/// The contents inside a `git-blob`, if it is one.
pub fn git_blob_contents(b: &[u8]) -> Option<&[u8]> {
    let rest = b.strip_prefix(b"blob ")?;
    let nul = rest.iter().position(|&c| c == 0)?;
    let n = std::str::from_utf8(&rest[..nul]).ok()?;
    let canonical = n == "0" || (!n.starts_with('0') && n.bytes().all(|c| c.is_ascii_digit()));
    let body = &rest[nul + 1..];
    (canonical && n.parse::<usize>().ok()? == body.len()).then_some(body)
}

fn git(b: &[u8]) -> std::result::Result<(), String> {
    git_blob_contents(b).map(|_| ()).ok_or_else(|| "not blob <n>\\0<contents>".into())
}

static FORMATS: LazyLock<Vec<Format>> = LazyLock::new(|| {
    vec![
        format("dir", include_str!("../../spec/formats/dir.md"), Kind::Dir(&dir::DIR)),
        format("raw", include_str!("../../spec/formats/raw.md"), Kind::Leaf(any)),
        format("exec", include_str!("../../spec/formats/exec.md"), Kind::Leaf(any)),
        format("symlink", include_str!("../../spec/formats/symlink.md"), Kind::Leaf(symlink)),
        format("git-blob", include_str!("../../spec/formats/git-blob.md"), Kind::Leaf(git)),
    ]
});

pub fn all() -> &'static [Format] {
    &FORMATS
}

pub fn by_alias(alias: &str) -> Option<&'static Format> {
    FORMATS.iter().find(|f| f.alias == alias)
}

pub fn by_id(id: &str) -> Option<&'static Format> {
    FORMATS.iter().find(|f| f.id == id)
}

pub fn metaformat_id() -> String {
    blake3().key(METAFORMAT.as_bytes())
}

/// Write the metaformat, and each format's spec and object, so that every
/// format id this crate writes can be looked up in `store`.
pub fn publish(store: &dyn Store) -> Result<()> {
    let mut blobs = vec![METAFORMAT.as_bytes()];
    for f in all() {
        blobs.push(f.spec.as_bytes());
        blobs.push(f.object.as_bytes());
    }
    for b in blobs {
        store.write(&blake3().key(b), b, &Expect::Absent).map_err(Error::Store)?;
    }
    Ok(())
}
