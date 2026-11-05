//! Sync (Stage 7, spec/sync.md): two stores converge with plain core ops.
//! Tooling walks formats from a reference and writes what the other store
//! lacks; there is no negotiation.
//!
//! Copies keep the closure invariant: a blob is written only after
//! everything it references, so a store that has a blob has all of it, and
//! a walk can stop at the first blob the destination already holds.

use crate::dir::Item;
use crate::formats::{self, Kind};
use crate::history;
use crate::{Error, Ref, Result, is_blob_key, read_blob, tree};
use cas_core::{Error as CoreError, Expect, Span, Store};

/// Whether `store` holds the blob `key`, without reading it.
pub fn has(store: &dyn Store, key: &str) -> Result<bool> {
    let mut end = key.as_bytes().to_vec();
    end.push(0);
    let page = store.list(&Span { from: Some(key.as_bytes().to_vec()), to: Some(end) })?;
    Ok(page.keys.iter().any(|k| k == key))
}

/// What a copy did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Copied {
    /// Blobs written to the destination.
    pub blobs: usize,
    /// Blobs skipped because the destination already had them.
    pub present: usize,
}

/// The references inside a blob, by its format. Unknown formats are
/// opaque, so their blobs are copied without following anything.
fn children(store: &dyn Store, r: &Ref) -> Result<Vec<Ref>> {
    match r.known_format().map(|f| (&f.kind, f.alias)) {
        Some((Kind::Dir(_), _)) => Ok(tree::load(store, r)?.into_iter().map(|Item { child, .. }| child).collect()),
        Some((_, "record")) => {
            let rec = history::read(store, r)?;
            Ok(std::iter::once(rec.version).chain(rec.parents).collect())
        }
        _ => Ok(Vec::new()),
    }
}

/// Copy one raw blob unless the destination has it.
fn copy_blob(src: &dyn Store, dst: &dyn Store, key: &str, n: &mut Copied) -> Result<()> {
    if has(dst, key)? {
        n.present += 1;
    } else {
        dst.write(key, &read_blob(src, key)?, &Expect::Absent)?;
        n.blobs += 1;
    }
    Ok(())
}

/// Copy the format object and spec of `format`, so the destination can
/// describe what it holds. Formats this crate does not know are skipped.
fn copy_format(src: &dyn Store, dst: &dyn Store, format: &str, n: &mut Copied) -> Result<()> {
    if let Some(f) = formats::by_id(format) {
        for bytes in [f.spec.as_bytes(), f.object().as_bytes()] {
            let key = cas_core::algo::lookup("blake3").unwrap().key(bytes);
            if has(dst, &key)? {
                n.present += 1;
            } else {
                dst.write(&key, bytes, &Expect::Absent)?;
                n.blobs += 1;
            }
        }
    } else if is_blob_key(format) && has(src, format)? {
        copy_blob(src, dst, format, n)?;
    }
    Ok(())
}

/// Copy everything reachable from `root` that `dst` lacks, children before
/// parents. Stops descending at blobs `dst` already has.
pub fn copy(src: &dyn Store, dst: &dyn Store, root: &Ref) -> Result<Copied> {
    let mut n = Copied::default();
    let mut formats_seen = std::collections::BTreeSet::new();
    // An explicit stack of (reference, children pushed yet).
    let mut stack = vec![(root.clone(), false)];
    while let Some((r, expanded)) = stack.pop() {
        if formats_seen.insert(r.format().to_string()) {
            copy_format(src, dst, r.format(), &mut n)?;
        }
        if expanded {
            copy_blob(src, dst, r.key(), &mut n)?;
            continue;
        }
        if has(dst, r.key())? {
            n.present += 1;
            continue;
        }
        stack.push((r.clone(), true));
        for c in children(src, &r)? {
            stack.push((c, false));
        }
    }
    Ok(n)
}

/// How a branch tag was synced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The destination already pointed at the same record.
    UpToDate,
    /// The destination had no such tag; it now points at the source's head.
    Created,
    /// The destination's head was behind; it moved forward.
    FastForward,
}

/// Make branch `tag` in `dst` point at its head in `src`, copying what is
/// needed. Moves the tag only when that loses no history: the destination
/// has no head, or its head is an ancestor of the source's. Otherwise
/// `Diverged`; merging is left to the caller.
pub fn push(src: &dyn Store, dst: &dyn Store, tag: &str) -> Result<(Outcome, Copied)> {
    let head = history::head(src, tag)?.ok_or_else(|| Error::Missing(format!("tag {tag} in the source")))?;
    let theirs = history::head(dst, tag)?;
    if theirs.as_ref() == Some(&head) {
        return Ok((Outcome::UpToDate, Copied::default()));
    }
    if let Some(t) = &theirs
        && !history::log(src, &head)?.iter().any(|(r, _)| r == t)
    {
        return Err(Error::Diverged(tag.to_string()));
    }
    let copied = copy(src, dst, &head)?;
    let expect = match &theirs {
        Some(t) => Expect::Value(t.to_string().into_bytes()),
        None => Expect::Absent,
    };
    match dst.write(tag, head.to_string().as_bytes(), &expect) {
        Ok(()) => Ok((if theirs.is_some() { Outcome::FastForward } else { Outcome::Created }, copied)),
        // The destination moved while we copied; the copy is still valid.
        Err(CoreError::Conflict) => Err(Error::Diverged(tag.to_string())),
        Err(e) => Err(e.into()),
    }
}
