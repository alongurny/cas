//! Three-way merge (spec/merge.md): combine two versions that both came from
//! a base, path by path, and line by line inside text files both changed.
//! Work follows the two diffs from the base, not the size of the tree.

use crate::history::{History, NodeId, Step};
use crate::{Change, Edit, Error, Node, Path, Result, View, by_alias, diff, put, read_blob, update};
use cas_core::Store;
use cas_core::algo::Algorithm;
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
    pub version: View,
    /// Empty when the merge is clean.
    pub conflicts: Vec<Conflict>,
}

fn changes(store: &dyn Store, a: &View, b: &View) -> Result<BTreeMap<Path, Change>> {
    Ok(diff(store, a, b, None)?.into_iter().map(|c| (c.path.clone(), c)).collect())
}

fn ancestors(p: &Path) -> impl Iterator<Item = Path> {
    std::iter::successors(p.parent(), Path::parent)
}

/// Merge `theirs` into `ours`, both descended from `base`.
pub fn merge_versions(
    store: &dyn Store,
    alg: &'static Algorithm,
    base: &View,
    ours: &View,
    theirs: &View,
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
fn merge_text(
    store: &dyn Store,
    alg: &'static Algorithm,
    base: &View,
    ours: &View,
    theirs: &View,
) -> Result<Option<View>> {
    let format = ours.known_format().map(|f| f.alias);
    if !matches!(format, Some("raw" | "exec")) || base.format() != ours.format() || theirs.format() != ours.format() {
        return Ok(None);
    }
    let read = |r: &View| -> Result<Option<String>> { Ok(String::from_utf8(read_blob(store, r.key())?).ok()) };
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

/// The version to merge against: the single best common ancestor, or,
/// after a criss-cross, a virtual base made by merging the ancestors
/// themselves (conflicts there resolve to the first ancestor's side).
fn base_version(
    store: &dyn Store,
    alg: &'static Algorithm,
    h: &History,
    a: &NodeId,
    b: &NodeId,
) -> Result<Option<View>> {
    let bases: Vec<NodeId> = h.merge_bases(a, b).into_iter().collect();
    let Some((first, rest)) = bases.split_first() else { return Ok(None) };
    let mut acc = first.clone();
    let mut version = first.version();
    for other in rest {
        let theirs = other.version();
        version = match base_version(store, alg, h, &acc, other)? {
            Some(base) => merge_versions(store, alg, &base, &version, &theirs)?.version,
            None => version,
        };
        acc = other.clone();
    }
    Ok(Some(version))
}

fn empty_dir(store: &dyn Store, alg: &'static Algorithm) -> Result<View> {
    crate::build(store, alg, by_alias("dir").unwrap(), &BTreeMap::new())
}

/// What a merge or rebase did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing to do.
    UpToDate,
    /// Done; the new node (the tip of a rebase).
    Done(NodeId),
    /// Nothing recorded: the merged version (ours at each conflict) and the
    /// conflicts, for the caller to resolve.
    Conflicts(Merged),
}

/// The union of two histories: every node of both, with the union of
/// their steps. Where both have a step between the same nodes, or a tag of
/// the same name, `ours` wins; docs likewise.
pub fn combine(ours: &History, theirs: &History) -> History {
    let mut h = theirs.clone();
    for (id, n) in &ours.nodes {
        let e = h.nodes.entry(id.clone()).or_default();
        e.after.extend(n.after.clone());
        if n.doc.is_some() {
            e.doc = n.doc.clone();
        }
    }
    h.tags.extend(ours.tags.clone());
    if ours.doc.is_some() {
        h.doc = ours.doc.clone();
    }
    h
}

/// Merge tips `a` and `b` of `h` into a new node following both, with
/// `step` from each. Rebasing is usually preferable (spec/history.md).
pub fn merge_nodes(
    store: &dyn Store,
    alg: &'static Algorithm,
    h: &mut History,
    a: &NodeId,
    b: &NodeId,
    step: &Step,
) -> Result<Outcome> {
    if h.follows(a, b) {
        return Ok(Outcome::UpToDate);
    }
    let base = match base_version(store, alg, h, a, b)? {
        Some(v) => v,
        // Unrelated lines merge against the empty tree.
        None => empty_dir(store, alg)?,
    };
    let merged = merge_versions(store, alg, &base, &a.version(), &b.version())?;
    if !merged.conflicts.is_empty() {
        return Ok(Outcome::Conflicts(merged));
    }
    let after = BTreeMap::from([(a.clone(), step.clone()), (b.clone(), step.clone())]);
    Ok(Outcome::Done(h.add(&merged.version, after, None)?))
}

/// The newest node of `mine` that `onto` also holds, and `mine`'s nodes
/// after it, oldest first. Those must form a single line.
fn private_part(mine: &History, onto: &History) -> Result<(Option<NodeId>, Vec<NodeId>)> {
    let tip = mine.tip().ok_or_else(|| Error::Diverged("the history has several tips".into()))?;
    let mut line = Vec::new();
    let mut at = Some(tip);
    while let Some(id) = at {
        if onto.nodes.contains_key(&id) {
            line.reverse();
            return Ok((Some(id), line));
        }
        let after = &mine.nodes[&id].after;
        if after.len() > 1 {
            return Err(Error::Diverged(format!("{id} is a merge; rebase replays a single line")));
        }
        at = after.keys().next().cloned();
        line.push(id);
    }
    line.reverse();
    Ok((None, line))
}

/// Replay the nodes `mine` has beyond `onto` on top of `onto`'s tip, as a
/// new history: `onto` plus the replayed line, steps and docs kept. Each
/// node is merged three-way: its predecessor as base, the replayed tip as
/// ours, the node as theirs.
pub fn rebase(
    store: &dyn Store,
    alg: &'static Algorithm,
    mine: &History,
    onto: &History,
) -> Result<(History, Outcome)> {
    let (base, line) = private_part(mine, onto)?;
    let mut h = onto.clone();
    let Some(mut tip) = onto.tip() else {
        if onto.nodes.is_empty() {
            return Ok((mine.clone(), Outcome::UpToDate));
        }
        return Err(Error::Diverged("the target has several tips".into()));
    };
    if line.is_empty() || base.as_ref() == Some(&tip) {
        return Ok((
            combine(mine, onto),
            if line.is_empty() { Outcome::UpToDate } else { Outcome::Done(mine.tip().unwrap()) },
        ));
    }
    let mut prev = match &base {
        Some(b) => b.version(),
        None => empty_dir(store, alg)?,
    };
    for id in &line {
        let n = &mine.nodes[id];
        let step = n.after.values().next().cloned().unwrap_or_default();
        let merged = merge_versions(store, alg, &prev, &tip.version(), &id.version())?;
        if !merged.conflicts.is_empty() {
            return Ok((h, Outcome::Conflicts(merged)));
        }
        tip = h.add(&merged.version, BTreeMap::from([(tip.clone(), step)]), n.doc.clone())?;
        prev = id.version();
    }
    Ok((h, Outcome::Done(tip)))
}

/// Generate the next step of `public` from `mine`: one node holding the
/// version of `mine`'s tip, after `public`'s tip, with `step`. `mine` must
/// already contain `public`'s tip (rebase first); its own nodes stay
/// private.
pub fn publish(mine: &History, public: &mut History, step: Step, doc: Option<String>) -> Result<Outcome> {
    let mtip = mine.tip().ok_or_else(|| Error::Diverged("the private history has several tips".into()))?;
    if let Some(ptip) = public.tip() {
        if !mine.follows(&mtip, &ptip) {
            return Err(Error::Diverged("the public history moved; rebase first".into()));
        }
        if ptip.version() == mtip.version() {
            return Ok(Outcome::UpToDate);
        }
    }
    Ok(Outcome::Done(public.append(&mtip.version(), step, doc)?))
}
