//! Versions as trees of directory blobs: build, update, lookup, walk and
//! diff. Each works through whatever directory format a node is in; only
//! the nodes on the paths involved are read or written.

use crate::dir::Item;
use crate::formats::{Format, Kind};
use crate::{Change, Edit, Error, Node, Path, Ref, Result, read_blob, write_blob};
use cas_core::Store;
use cas_core::algo::Algorithm;
use std::cmp::Ordering;
use std::collections::BTreeMap;

/// Entries in canonical order.
pub type Walk<'s> = Box<dyn Iterator<Item = Result<(Path, Node)>> + 's>;

fn dir_format(r: &Ref) -> Result<&'static Format> {
    match r.known_format() {
        Some(f @ Format { kind: Kind::Dir(_), .. }) => Ok(f),
        _ => Err(Error::UnknownFormat(format!("{} is not a directory format", r.format()))),
    }
}

pub(crate) fn load(store: &dyn Store, r: &Ref) -> Result<Vec<Item>> {
    let Kind::Dir(d) = &dir_format(r)?.kind else { unreachable!() };
    d.decode(r.key(), &read_blob(store, r.key())?)
}

/// Write a directory in `format` unless it is `old` already.
fn put(store: &dyn Store, alg: &'static Algorithm, format: &Format, items: &[Item], old: Option<&Ref>) -> Result<Ref> {
    let Kind::Dir(d) = &format.kind else { unreachable!() };
    let bytes = d.encode(items);
    if let Some(old) = old
        && old.key() == alg.key(&bytes)
        && old.format() == format.id()
    {
        return Ok(old.clone());
    }
    Ok(Ref { format: format.id().to_string(), key: write_blob(store, alg, &bytes)? })
}

type Group<'e, T> = Vec<(&'e [&'e str], &'e T)>;

/// Split path-sorted `(names, value)` pairs into runs sharing a first name.
fn groups<'e, T>(pairs: &'e [(Vec<&'e str>, &'e T)]) -> Vec<(&'e str, Group<'e, T>)> {
    let mut out: Vec<(&str, Group<T>)> = Vec::new();
    for (names, v) in pairs {
        match out.last_mut() {
            Some((name, g)) if *name == names[0] => g.push((&names[1..], *v)),
            _ => out.push((names[0], vec![(&names[1..], *v)])),
        }
    }
    out
}

fn owned<'e, T>(pairs: &[(&'e [&'e str], &'e T)]) -> Vec<(Vec<&'e str>, &'e T)> {
    pairs.iter().map(|(n, v)| (n.to_vec(), *v)).collect()
}

fn split<T>(map: &BTreeMap<Path, T>) -> Vec<(Vec<&str>, &T)> {
    map.iter().map(|(p, v)| (p.names().collect(), v)).collect()
}

struct Writer<'a> {
    store: &'a dyn Store,
    alg: &'static Algorithm,
}

impl Writer<'_> {
    fn build_dir(&self, format: &Format, dir: Option<&Path>, pairs: &[(&[&str], &Node)]) -> Result<Ref> {
        let mut items = Vec::new();
        for (name, group) in groups(&owned(pairs)) {
            let path = Path::child(dir, name);
            let exact = group.iter().find(|(rest, _)| rest.is_empty()).map(|(_, n)| *n);
            let deeper: Vec<_> = group.iter().filter(|(rest, _)| !rest.is_empty()).copied().collect();
            let child = match exact {
                Some(Node::File(_)) if !deeper.is_empty() => return Err(Error::PathConflict(path)),
                Some(Node::File(r)) => r.clone(),
                _ => self.build_dir(format, Some(&path), &deeper)?,
            };
            items.push(Item { name: name.to_string(), child });
        }
        put(self.store, self.alg, format, &items, None)
    }

    /// Apply edits relative to `dir` to the directory `node` (`None`: a new
    /// one in `format`).
    fn update_dir(
        &self,
        format: &'static Format,
        dir: Option<&Path>,
        node: Option<&Ref>,
        edits: &[(&[&str], &Edit)],
    ) -> Result<Ref> {
        let (format, mut items) = match node {
            Some(r) => (dir_format(r)?, load(self.store, r)?),
            None => (format, Vec::new()),
        };
        for (name, group) in groups(&owned(edits)) {
            let path = Path::child(dir, name);
            let leaf = group.iter().find(|(rest, _)| rest.is_empty()).map(|(_, e)| *e);
            let mut deeper: Vec<_> = group.iter().filter(|(rest, _)| !rest.is_empty()).copied().collect();
            let sets_deeper = deeper.iter().any(|(_, e)| matches!(e, Edit::Set(_)));
            let pos = items.binary_search_by(|it| it.name.as_str().cmp(name));
            let mut cur = pos.ok().map(|p| items[p].child.clone());
            let mut make_dir = false;
            match leaf {
                Some(Edit::Remove) => cur = None,
                Some(Edit::Set(Node::File(_))) if sets_deeper => return Err(Error::PathConflict(path)),
                Some(Edit::Set(Node::File(r))) => {
                    cur = Some(r.clone());
                    deeper.clear();
                }
                Some(Edit::Set(Node::Dir)) if !cur.as_ref().is_some_and(Ref::is_dir) => {
                    cur = None;
                    make_dir = true;
                }
                Some(Edit::Set(Node::Dir)) | None => {}
            }
            if !deeper.is_empty() || make_dir {
                cur = match cur {
                    Some(r) if r.is_dir() => Some(self.update_dir(format, Some(&path), Some(&r), &deeper)?),
                    Some(_) if sets_deeper => return Err(Error::PathConflict(path)),
                    None if sets_deeper || make_dir => Some(self.update_dir(format, Some(&path), None, &deeper)?),
                    other => other,
                };
            }
            match (pos, cur) {
                (Ok(p), Some(child)) => items[p].child = child,
                (Ok(p), None) => {
                    items.remove(p);
                }
                (Err(p), Some(child)) => items.insert(p, Item { name: name.to_string(), child }),
                (Err(_), None) => {}
            }
        }
        put(self.store, self.alg, format, &items, node)
    }
}

/// Write a version holding `entries`, with every directory in `format`.
/// Parent directories that `entries` leaves out are created.
pub fn build(
    store: &dyn Store,
    alg: &'static Algorithm,
    format: &'static Format,
    entries: &BTreeMap<Path, Node>,
) -> Result<Ref> {
    if !matches!(format.kind, Kind::Dir(_)) {
        return Err(Error::UnknownFormat(format!("{} is not a directory format", format.alias)));
    }
    let owned = split(entries);
    let pairs: Vec<(&[&str], &Node)> = owned.iter().map(|(n, e)| (n.as_slice(), *e)).collect();
    Writer { store, alg }.build_dir(format, None, &pairs)
}

/// Apply `edits` to the version at `root`: all removes, then all sets in
/// canonical order. New directories take the format of their parent.
pub fn update(store: &dyn Store, alg: &'static Algorithm, root: &Ref, edits: &BTreeMap<Path, Edit>) -> Result<Ref> {
    let format = dir_format(root)?;
    let owned = split(edits);
    let pairs: Vec<(&[&str], &Edit)> = owned.iter().map(|(n, e)| (n.as_slice(), *e)).collect();
    Writer { store, alg }.update_dir(format, None, Some(root), &pairs)
}

/// What a version holds at a path.
enum At {
    Nothing,
    File(Ref),
    Dir(Ref),
}

impl At {
    fn of(r: Ref) -> At {
        if r.is_dir() { At::Dir(r) } else { At::File(r) }
    }

    fn node(&self) -> Option<Node> {
        match self {
            At::Nothing => None,
            At::File(r) => Some(Node::File(r.clone())),
            At::Dir(_) => Some(Node::Dir),
        }
    }
}

fn resolve(store: &dyn Store, root: &Ref, path: Option<&Path>) -> Result<At> {
    dir_format(root)?;
    let Some(path) = path else { return Ok(At::Dir(root.clone())) };
    let mut at = At::Dir(root.clone());
    for name in path.names() {
        let At::Dir(r) = at else { return Ok(At::Nothing) };
        let items = load(store, &r)?;
        at = match items.binary_search_by(|it| it.name.as_str().cmp(name)) {
            Ok(p) => At::of(items[p].child.clone()),
            Err(_) => return Ok(At::Nothing),
        };
    }
    Ok(at)
}

/// The object at `path`: a file's reference or a directory's own.
pub fn object_at(store: &dyn Store, root: &Ref, path: &Path) -> Result<Option<Ref>> {
    Ok(match resolve(store, root, Some(path))? {
        At::Nothing => None,
        At::File(r) | At::Dir(r) => Some(r),
    })
}

pub fn lookup(store: &dyn Store, root: &Ref, path: &Path) -> Result<Option<Node>> {
    Ok(resolve(store, root, Some(path))?.node())
}

/// Depth-first walk under one directory.
struct TreeWalk<'s> {
    store: &'s dyn Store,
    stack: Vec<(Option<Path>, std::vec::IntoIter<Item>)>,
    pending: Option<(Option<Path>, Ref)>,
    failed: bool,
}

impl<'s> TreeWalk<'s> {
    fn new(store: &'s dyn Store, dir: Option<Path>, r: Ref) -> TreeWalk<'s> {
        TreeWalk { store, stack: Vec::new(), pending: Some((dir, r)), failed: false }
    }
}

impl Iterator for TreeWalk<'_> {
    type Item = Result<(Path, Node)>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        loop {
            if let Some((dir, r)) = self.pending.take() {
                match load(self.store, &r) {
                    Ok(items) => self.stack.push((dir, items.into_iter())),
                    Err(e) => {
                        self.failed = true;
                        return Some(Err(e));
                    }
                }
            }
            let (dir, items) = self.stack.last_mut()?;
            let Some(it) = items.next() else {
                self.stack.pop();
                continue;
            };
            let path = Path::child(dir.as_ref(), &it.name);
            return Some(Ok(match At::of(it.child) {
                At::Dir(r) => {
                    self.pending = Some((Some(path.clone()), r));
                    (path, Node::Dir)
                }
                At::File(r) => (path, Node::File(r)),
                At::Nothing => unreachable!(),
            }));
        }
    }
}

/// What is at or under `prefix` (all of the version for `None`), in
/// canonical order, reading only the directories it returns.
pub fn walk<'s>(store: &'s dyn Store, root: &Ref, prefix: Option<&Path>) -> Walk<'s> {
    let at = match resolve(store, root, prefix) {
        Ok(at) => at,
        Err(e) => return Box::new(std::iter::once(Err(e))),
    };
    let here = prefix.cloned().zip(at.node()).map(Ok);
    match at {
        At::Dir(r) => Box::new(here.into_iter().chain(TreeWalk::new(store, prefix.cloned(), r))),
        _ => Box::new(here.into_iter()),
    }
}

fn all_under(store: &dyn Store, path: &Path, r: Ref, old: bool, out: &mut Vec<Change>) -> Result<()> {
    for item in TreeWalk::new(store, Some(path.clone()), r) {
        let (path, node) = item?;
        let (o, n) = if old { (Some(node), None) } else { (None, Some(node)) };
        out.push(Change { path, old: o, new: n });
    }
    Ok(())
}

fn diff_at(store: &dyn Store, path: Option<&Path>, a: At, b: At, out: &mut Vec<Change>) -> Result<()> {
    match (a, b) {
        (At::Dir(x), At::Dir(y)) if x == y => {}
        (At::Dir(x), At::Dir(y)) => {
            let (mut ia, mut ib) = (load(store, &x)?.into_iter().peekable(), load(store, &y)?.into_iter().peekable());
            loop {
                let order = match (ia.peek(), ib.peek()) {
                    (None, None) => break,
                    (Some(_), None) => Ordering::Less,
                    (None, Some(_)) => Ordering::Greater,
                    (Some(p), Some(q)) => p.name.cmp(&q.name),
                };
                let (pa, pb) = match order {
                    Ordering::Less => (ia.next(), None),
                    Ordering::Greater => (None, ib.next()),
                    Ordering::Equal => (ia.next(), ib.next()),
                };
                let name = pa.as_ref().or(pb.as_ref()).unwrap().name.clone();
                let at = |it: Option<Item>| it.map_or(At::Nothing, |i| At::of(i.child));
                diff_at(store, Some(&Path::child(path, &name)), at(pa), at(pb), out)?;
            }
        }
        (a, b) => {
            // Only the root has no path, and it is a directory on both sides.
            let path = path.unwrap();
            let (old, new) = (a.node(), b.node());
            if old != new {
                out.push(Change { path: path.clone(), old, new });
            }
            // A path's own change sorts before the paths under it.
            if let At::Dir(r) = a {
                all_under(store, path, r, true, out)?;
            }
            if let At::Dir(r) = b {
                all_under(store, path, r, false, out)?;
            }
        }
    }
    Ok(())
}

/// Differences at or under `prefix`, in canonical order. Descends only into
/// directories whose references differ.
pub fn diff(store: &dyn Store, a: &Ref, b: &Ref, prefix: Option<&Path>) -> Result<Vec<Change>> {
    let mut out = Vec::new();
    diff_at(store, prefix, resolve(store, a, prefix)?, resolve(store, b, prefix)?, &mut out)?;
    Ok(out)
}

/// Diff two canonical-order walks by merging them. Slower than [`diff`],
/// which skips equal directories, but works for any two walks.
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
