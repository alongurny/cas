//! Three-way merge (spec/merge.md): combine two versions that both came from
//! a base, path by path, and line by line inside text files both changed.
//! Work follows the two diffs from the base, not the size of the tree.

use crate::history::{self, Record};
use crate::uni::Value;
use crate::{Change, Edit, Error, Node, Path, Ref, Result, by_alias, diff, put, read_blob, update};
use cas_core::algo::Algorithm;
use cas_core::{Expect, Store};
use std::collections::{BTreeMap, BTreeSet};

/// A path both sides changed differently. The merged version keeps ours.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    pub path: Path,
    pub base: Option<Node>,
    pub ours: Option<Node>,
    pub theirs: Option<Node>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Merged {
    pub version: Ref,
    /// Empty when the merge is clean.
    pub conflicts: Vec<Conflict>,
}

fn changes(store: &dyn Store, a: &Ref, b: &Ref) -> Result<BTreeMap<Path, Change>> {
    Ok(diff(store, a, b, None)?.into_iter().map(|c| (c.path.clone(), c)).collect())
}

fn ancestors(p: &Path) -> impl Iterator<Item = Path> {
    std::iter::successors(p.parent(), Path::parent)
}

/// Merge `theirs` into `ours`, both descended from `base`.
pub fn merge_versions(
    store: &dyn Store,
    alg: &'static Algorithm,
    base: &Ref,
    ours: &Ref,
    theirs: &Ref,
) -> Result<Merged> {
    let mine = changes(store, base, ours)?;
    let other = changes(store, base, theirs)?;
    let mut edits = BTreeMap::new();
    let mut conflicts = Vec::new();
    let mut conflicted: BTreeSet<Path> = BTreeSet::new();
    for (p, t) in &other {
        let clash = match mine.get(p) {
            // Both changed this path: fine only if they agree.
            Some(o) => o.new != t.new,
            None => {
                // We turned a directory above it into something else.
                let above = ancestors(p).any(|q| mine.get(&q).is_some_and(|o| o.new != Some(Node::Dir)));
                // They removed or replaced a directory we changed inside.
                let below = t.new != Some(Node::Dir)
                    && mine
                        .range(p.clone()..)
                        .take_while(|(q, _)| q.starts_with(p))
                        .any(|(q, o)| other.get(q) != Some(o));
                above || below
            }
        };
        if !clash {
            if !mine.contains_key(p) {
                edits.insert(p.clone(), t.new.clone().map_or(Edit::Remove, Edit::Set));
            }
            continue;
        }
        let ours_now = mine.get(p).map_or(t.old.clone(), |o| o.new.clone());
        // Both edited the same text file: try a line merge.
        if let (Some(Node::File(b)), Some(Node::File(o)), Some(Node::File(n))) = (&t.old, &ours_now, &t.new)
            && let Some(merged) = merge_text(store, alg, b, o, n)?
        {
            edits.insert(p.clone(), Edit::Set(Node::File(merged)));
            continue;
        }
        if conflicted.insert(p.clone()) {
            conflicts.push(Conflict { path: p.clone(), base: t.old.clone(), ours: ours_now, theirs: t.new.clone() });
        }
    }
    // Never apply their change under a conflicted path, nor remove a
    // conflicted path's directory.
    edits.retain(|p, _| !conflicted.iter().any(|c| p.starts_with(c) || c.starts_with(p)));
    let version = update(store, alg, ours, &edits)?;
    Ok(Merged { version, conflicts })
}

/// Line-level three-way merge of text files in the same format (`raw` or
/// `exec`). `None` when they are not text or their edits overlap.
fn merge_text(store: &dyn Store, alg: &'static Algorithm, base: &Ref, ours: &Ref, theirs: &Ref) -> Result<Option<Ref>> {
    let format = ours.known_format().map(|f| f.alias);
    if !matches!(format, Some("raw" | "exec")) || base.format() != ours.format() || theirs.format() != ours.format() {
        return Ok(None);
    }
    let read = |r: &Ref| -> Result<Option<String>> { Ok(String::from_utf8(read_blob(store, r.key())?).ok()) };
    let (Some(b), Some(o), Some(t)) = (read(base)?, read(ours)?, read(theirs)?) else { return Ok(None) };
    match diff3(&lines(&b), &lines(&o), &lines(&t)) {
        Some(merged) => Ok(Some(put(store, alg, format.unwrap(), merged.concat().as_bytes())?)),
        None => Ok(None),
    }
}

/// Lines, each keeping its newline.
fn lines(s: &str) -> Vec<&str> {
    s.split_inclusive('\n').collect()
}

/// For each line of `a`, the index of the line of `b` it matches in a
/// longest common subsequence.
fn lcs_map(a: &[&str], b: &[&str]) -> Vec<Option<usize>> {
    let (n, m) = (a.len(), b.len());
    let mut t = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            t[i][j] = if a[i] == b[j] { t[i + 1][j + 1] + 1 } else { t[i + 1][j].max(t[i][j + 1]) };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, vec![None; n]);
    while i < n && j < m {
        if a[i] == b[j] {
            out[i] = Some(j);
            i += 1;
            j += 1;
        } else if t[i + 1][j] >= t[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// Classic diff3: walk base lines kept by both sides as sync points; take
/// each chunk between them from whichever side changed it. `None` on a
/// chunk both changed differently.
pub fn diff3<'a>(base: &[&'a str], ours: &[&'a str], theirs: &[&'a str]) -> Option<Vec<&'a str>> {
    let (mo, mt) = (lcs_map(base, ours), lcs_map(base, theirs));
    let mut out = Vec::new();
    let (mut b0, mut o0, mut t0) = (0, 0, 0);
    let mut i = 0;
    loop {
        // Next base line both sides kept.
        while i < base.len() && (mo[i].is_none() || mt[i].is_none()) {
            i += 1;
        }
        let (b1, o1, t1) =
            if i < base.len() { (i, mo[i].unwrap(), mt[i].unwrap()) } else { (base.len(), ours.len(), theirs.len()) };
        let (cb, co, ct) = (&base[b0..b1], &ours[o0..o1], &theirs[t0..t1]);
        if co == cb {
            out.extend_from_slice(ct);
        } else if ct == cb || co == ct {
            out.extend_from_slice(co);
        } else {
            return None;
        }
        if i >= base.len() {
            return Some(out);
        }
        out.push(base[i]);
        (b0, o0, t0) = (b1 + 1, o1 + 1, t1 + 1);
        i += 1;
    }
}

/// The base to merge against: the single best common ancestor, or, after a
/// criss-cross, a virtual base made by merging the ancestors themselves
/// (conflicts there resolve to the first ancestor's side).
fn base_version(store: &dyn Store, alg: &'static Algorithm, a: &Ref, b: &Ref) -> Result<Option<Ref>> {
    let bases: Vec<Ref> = history::merge_bases(store, a, b)?.into_iter().collect();
    let Some((first, rest)) = bases.split_first() else { return Ok(None) };
    let mut acc = first.clone();
    let mut version = history::read(store, first)?.version;
    for other in rest {
        let inner = base_version(store, alg, &acc, other)?;
        let theirs = history::read(store, other)?.version;
        version = match inner {
            Some(base) => merge_versions(store, alg, &base, &version, &theirs)?.version,
            None => version,
        };
        acc = other.clone();
    }
    Ok(Some(version))
}

/// What merging a branch did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The other head is already in our history.
    UpToDate,
    /// Our head was in theirs; the branch now points at theirs.
    FastForward(Ref),
    /// A new record with both heads as parents.
    Merged(Ref),
    /// Nothing recorded; the merged version (ours at each conflict) and the
    /// conflicts, for the caller to resolve and commit.
    Conflicts(Merged),
}

/// Merge record `theirs` into branch `tag`.
pub fn merge_branch(
    store: &dyn Store,
    alg: &'static Algorithm,
    tag: &str,
    theirs: &Ref,
    meta: BTreeMap<Value, Value>,
) -> Result<Outcome> {
    let ours = history::head(store, tag)?.ok_or_else(|| Error::Missing(format!("tag {tag}")))?;
    let set = |new: &Ref| {
        store.write(tag, new.to_string().as_bytes(), &Expect::Value(ours.to_string().into_bytes())).map_err(Error::from)
    };
    if history::log(store, &ours)?.iter().any(|(r, _)| r == theirs) {
        return Ok(Outcome::UpToDate);
    }
    if history::log(store, theirs)?.iter().any(|(r, _)| *r == ours) {
        set(theirs)?;
        return Ok(Outcome::FastForward(theirs.clone()));
    }
    let (ov, tv) = (history::read(store, &ours)?.version, history::read(store, theirs)?.version);
    let base = match base_version(store, alg, &ours, theirs)? {
        Some(b) => b,
        // Unrelated histories merge against the empty tree.
        None => crate::build(store, alg, by_alias("dir").unwrap(), &BTreeMap::new())?,
    };
    let merged = merge_versions(store, alg, &base, &ov, &tv)?;
    if !merged.conflicts.is_empty() {
        return Ok(Outcome::Conflicts(merged));
    }
    let rec = Record { version: merged.version, parents: [ours.clone(), theirs.clone()].into(), meta };
    let r = history::write(store, alg, &rec)?;
    set(&r)?;
    Ok(Outcome::Merged(r))
}
