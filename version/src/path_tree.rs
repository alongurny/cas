//! Shape `path/1`: one node per directory, like git. A node lists its
//! entries by name; a `tree` entry points to the node of a subdirectory.
//!
//! ```text
//! cas-tree path/1\n
//! <type> <key> <name>\n      zero or more, names strictly ascending
//! ```
//!
//! `<type>` is `file`, `exec`, `link` or `tree`. Only the root may be empty,
//! since a version holds files, not directories.

use crate::{
    Change, Edit, Entry, Error, MAGIC, Mode, Path, Result, Shape, Walk, is_blob_key, malformed, read_blob, valid_name,
    write_blob,
};
use cas_core::Store;
use cas_core::algo::Algorithm;
use std::cmp::Ordering;
use std::collections::BTreeMap;

pub struct PathTree;

const NAME: &str = "path/1";

#[derive(Clone, Debug, PartialEq, Eq)]
enum Kind {
    Leaf(Entry),
    Tree(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Item {
    name: String,
    kind: Kind,
}

fn header() -> String {
    format!("{MAGIC}{NAME}\n")
}

fn encode(items: &[Item]) -> Vec<u8> {
    let mut out = header();
    for it in items {
        let (ty, key) = match &it.kind {
            Kind::Leaf(e) => (e.mode.as_str(), e.key()),
            Kind::Tree(k) => ("tree", k.as_str()),
        };
        out.push_str(&format!("{ty} {key} {}\n", it.name));
    }
    out.into_bytes()
}

/// Parse a node, accepting only the canonical encoding.
fn decode(key: &str, bytes: &[u8]) -> Result<Vec<Item>> {
    let text = std::str::from_utf8(bytes).map_err(|_| malformed(key, "not UTF-8"))?;
    let body = text.strip_prefix(&header()).ok_or_else(|| malformed(key, format!("not a {NAME} node")))?;
    let mut items: Vec<Item> = Vec::new();
    for line in body.split_inclusive('\n') {
        let line = line.strip_suffix('\n').ok_or_else(|| malformed(key, "last line has no newline"))?;
        let mut parts = line.splitn(3, ' ');
        let (Some(ty), Some(k), Some(name)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(malformed(key, format!("bad line {line:?}")));
        };
        if !is_blob_key(k) {
            return Err(malformed(key, format!("bad key {k:?}")));
        }
        if !valid_name(name) {
            return Err(malformed(key, format!("bad name {name:?}")));
        }
        let kind = match ty {
            "tree" => Kind::Tree(k.to_string()),
            _ => Kind::Leaf(Entry {
                key: k.to_string(),
                mode: Mode::parse(ty).ok_or_else(|| malformed(key, format!("bad type {ty:?}")))?,
            }),
        };
        if items.last().is_some_and(|prev| prev.name.as_str() >= name) {
            return Err(malformed(key, format!("{name:?} out of order")));
        }
        items.push(Item { name: name.to_string(), kind });
    }
    Ok(items)
}

fn load(store: &dyn Store, key: &str) -> Result<Vec<Item>> {
    decode(key, &read_blob(store, key)?)
}

/// A subtree node, which must not be empty.
fn load_sub(store: &dyn Store, key: &str) -> Result<Vec<Item>> {
    let items = load(store, key)?;
    if items.is_empty() {
        return Err(malformed(key, "empty subtree"));
    }
    Ok(items)
}

/// Write a node unless it is `old` already; returns its key.
fn put(store: &dyn Store, alg: &'static Algorithm, items: &[Item], old: Option<&str>) -> Result<String> {
    let bytes = encode(items);
    if old.is_some_and(|k| k == alg.key(&bytes)) {
        return Ok(old.unwrap().to_string());
    }
    write_blob(store, alg, &bytes)
}

type Group<'e, T> = Vec<(&'e [&'e str], &'e T)>;

/// Split path-sorted `(names, value)` pairs into runs sharing a first name.
fn groups<'e, T>(pairs: &'e [(Vec<&'e str>, &'e T)]) -> Vec<(&'e str, Group<'e, T>)> {
    let mut out: Vec<(&str, Group<T>)> = Vec::new();
    for (names, v) in pairs {
        let first = names[0];
        match out.last_mut() {
            Some((name, g)) if *name == first => g.push((&names[1..], *v)),
            _ => out.push((first, vec![(&names[1..], *v)])),
        }
    }
    out
}

fn split<T>(map: &BTreeMap<Path, T>) -> Vec<(Vec<&str>, &T)> {
    map.iter().map(|(p, v)| (p.names().collect(), v)).collect()
}

/// `pairs` are relative to `dir`, in canonical order, each non-empty.
fn build_dir(
    store: &dyn Store,
    alg: &'static Algorithm,
    dir: Option<&Path>,
    pairs: &[(&[&str], &Entry)],
) -> Result<String> {
    let owned: Vec<(Vec<&str>, &Entry)> = pairs.iter().map(|(n, e)| (n.to_vec(), *e)).collect();
    let mut items = Vec::new();
    for (name, group) in groups(&owned) {
        let path = Path::child(dir, name);
        let kind = match group.as_slice() {
            [([], e)] => Kind::Leaf((*e).clone()),
            [([], _), ..] => return Err(Error::PathConflict(path)),
            _ => Kind::Tree(build_dir(store, alg, Some(&path), &group)?),
        };
        items.push(Item { name: name.to_string(), kind });
    }
    put(store, alg, &items, None)
}

/// Apply edits relative to `dir` to the node `node` (`None`: empty).
/// Returns `None` when a subdirectory ends up empty.
fn update_dir(
    store: &dyn Store,
    alg: &'static Algorithm,
    dir: Option<&Path>,
    node: Option<&str>,
    edits: &[(&[&str], &Edit)],
) -> Result<Option<String>> {
    let mut items = match (node, dir) {
        (None, _) => Vec::new(),
        (Some(k), None) => load(store, k)?,
        (Some(k), Some(_)) => load_sub(store, k)?,
    };
    let owned: Vec<(Vec<&str>, &Edit)> = edits.iter().map(|(n, e)| (n.to_vec(), *e)).collect();
    for (name, group) in groups(&owned) {
        let path = Path::child(dir, name);
        let leaf = group.iter().find(|(rest, _)| rest.is_empty()).map(|(_, e)| *e);
        let deeper: Vec<(&[&str], &Edit)> = group.iter().filter(|(rest, _)| !rest.is_empty()).copied().collect();
        let sets_deeper = deeper.iter().any(|(_, e)| matches!(e, Edit::Set(_)));
        let pos = items.binary_search_by(|it| it.name.as_str().cmp(name));
        let existing = pos.ok().map(|p| items[p].kind.clone());
        let new = match leaf {
            Some(Edit::Set(_)) if sets_deeper => return Err(Error::PathConflict(path)),
            Some(Edit::Set(e)) => Some(Kind::Leaf(e.clone())),
            // Removed, then rebuilt from whatever is set below it.
            Some(Edit::Remove) if deeper.is_empty() => None,
            Some(Edit::Remove) => update_dir(store, alg, Some(&path), None, &deeper)?.map(Kind::Tree),
            None => match existing.clone() {
                Some(Kind::Leaf(_)) if sets_deeper => return Err(Error::PathConflict(path)),
                Some(Kind::Leaf(e)) => Some(Kind::Leaf(e)),
                Some(Kind::Tree(k)) => update_dir(store, alg, Some(&path), Some(&k), &deeper)?.map(Kind::Tree),
                None => update_dir(store, alg, Some(&path), None, &deeper)?.map(Kind::Tree),
            },
        };
        match (pos, new) {
            (Ok(p), Some(kind)) => items[p].kind = kind,
            (Ok(p), None) => {
                items.remove(p);
            }
            (Err(p), Some(kind)) => items.insert(p, Item { name: name.to_string(), kind }),
            (Err(_), None) => {}
        }
    }
    if items.is_empty() && dir.is_some() {
        return Ok(None);
    }
    put(store, alg, &items, node).map(Some)
}

/// Where a path lands in a version.
enum At {
    Nothing,
    Leaf(Entry),
    Tree(String),
}

fn resolve(store: &dyn Store, root: &str, path: Option<&Path>) -> Result<At> {
    let Some(path) = path else { return Ok(At::Tree(root.to_string())) };
    let mut items = load(store, root)?;
    let mut names = path.names().peekable();
    while let Some(name) = names.next() {
        let Ok(p) = items.binary_search_by(|it| it.name.as_str().cmp(name)) else { return Ok(At::Nothing) };
        match (&items[p].kind, names.peek()) {
            (Kind::Leaf(e), None) => return Ok(At::Leaf(e.clone())),
            (Kind::Leaf(_), Some(_)) => return Ok(At::Nothing),
            (Kind::Tree(k), None) => return Ok(At::Tree(k.clone())),
            (Kind::Tree(k), Some(_)) => items = load_sub(store, k)?,
        }
    }
    unreachable!("paths have at least one name")
}

/// Depth-first walk of one node and everything under it.
struct TreeWalk<'s> {
    store: &'s dyn Store,
    stack: Vec<(Option<Path>, std::vec::IntoIter<Item>)>,
    pending: Option<Result<(Option<Path>, String)>>,
    failed: bool,
}

impl<'s> TreeWalk<'s> {
    fn new(store: &'s dyn Store, dir: Option<Path>, key: String) -> TreeWalk<'s> {
        TreeWalk { store, stack: Vec::new(), pending: Some(Ok((dir, key))), failed: false }
    }

    fn empty(store: &'s dyn Store) -> TreeWalk<'s> {
        TreeWalk { store, stack: Vec::new(), pending: None, failed: false }
    }

    fn fail(store: &'s dyn Store, e: Error) -> TreeWalk<'s> {
        TreeWalk { store, stack: Vec::new(), pending: Some(Err(e)), failed: false }
    }
}

impl Iterator for TreeWalk<'_> {
    type Item = Result<(Path, Entry)>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        loop {
            if let Some(next) = self.pending.take() {
                let loaded = next.and_then(|(dir, key)| {
                    let items = if dir.is_some() { load_sub(self.store, &key)? } else { load(self.store, &key)? };
                    Ok((dir, items))
                });
                match loaded {
                    Ok((dir, items)) => self.stack.push((dir, items.into_iter())),
                    Err(e) => {
                        self.failed = true;
                        return Some(Err(e));
                    }
                }
            }
            let (dir, items) = self.stack.last_mut()?;
            match items.next() {
                None => {
                    self.stack.pop();
                }
                Some(it) => {
                    let path = Path::child(dir.as_ref(), &it.name);
                    match it.kind {
                        Kind::Leaf(e) => return Some(Ok((path, e))),
                        Kind::Tree(k) => self.pending = Some(Ok((Some(path), k))),
                    }
                }
            }
        }
    }
}

fn walk_at<'s>(store: &'s dyn Store, prefix: Option<&Path>, at: At) -> Walk<'s> {
    match at {
        At::Nothing => Box::new(TreeWalk::empty(store)),
        At::Leaf(e) => Box::new(std::iter::once(Ok((prefix.unwrap().clone(), e)))),
        At::Tree(k) => Box::new(TreeWalk::new(store, prefix.cloned(), k)),
    }
}

fn all_of(store: &dyn Store, path: &Path, key: &str, old: bool, out: &mut Vec<Change>) -> Result<()> {
    for r in TreeWalk::new(store, Some(path.clone()), key.to_string()) {
        let (path, e) = r?;
        let (o, n) = if old { (Some(e), None) } else { (None, Some(e)) };
        out.push(Change { path, old: o, new: n });
    }
    Ok(())
}

/// Diff what `a` and `b` hold at `path`, skipping equal subtrees.
fn diff_at(store: &dyn Store, path: Option<&Path>, a: At, b: At, out: &mut Vec<Change>) -> Result<()> {
    let leaf = |old: Option<Entry>, new: Option<Entry>, out: &mut Vec<Change>| {
        if old != new {
            out.push(Change { path: path.unwrap().clone(), old, new });
        }
    };
    match (a, b) {
        (At::Nothing, At::Nothing) => {}
        (At::Leaf(x), At::Leaf(y)) => leaf(Some(x), Some(y), out),
        (At::Leaf(x), At::Nothing) => leaf(Some(x), None, out),
        (At::Nothing, At::Leaf(y)) => leaf(None, Some(y), out),
        // A path's own change sorts before the paths under it.
        (At::Leaf(x), At::Tree(k)) => {
            leaf(Some(x), None, out);
            all_of(store, path.unwrap(), &k, false, out)?;
        }
        (At::Tree(k), At::Leaf(y)) => {
            leaf(None, Some(y), out);
            all_of(store, path.unwrap(), &k, true, out)?;
        }
        (At::Tree(k), At::Nothing) => all_of(store, path.unwrap(), &k, true, out)?,
        (At::Nothing, At::Tree(k)) => all_of(store, path.unwrap(), &k, false, out)?,
        (At::Tree(x), At::Tree(y)) if x == y => {}
        (At::Tree(x), At::Tree(y)) => {
            let (load_a, load_b) = if path.is_some() {
                (load_sub(store, &x)?, load_sub(store, &y)?)
            } else {
                (load(store, &x)?, load(store, &y)?)
            };
            let (mut ia, mut ib) = (load_a.into_iter().peekable(), load_b.into_iter().peekable());
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
                let at = |it: Option<Item>| match it.map(|i| i.kind) {
                    None => At::Nothing,
                    Some(Kind::Leaf(e)) => At::Leaf(e),
                    Some(Kind::Tree(k)) => At::Tree(k),
                };
                diff_at(store, Some(&Path::child(path, &name)), at(pa), at(pb), out)?;
            }
        }
    }
    Ok(())
}

impl Shape for PathTree {
    fn name(&self) -> &'static str {
        NAME
    }

    fn build(&self, store: &dyn Store, alg: &'static Algorithm, entries: &BTreeMap<Path, Entry>) -> Result<String> {
        let owned = split(entries);
        let pairs: Vec<(&[&str], &Entry)> = owned.iter().map(|(n, e)| (n.as_slice(), *e)).collect();
        build_dir(store, alg, None, &pairs)
    }

    fn update(
        &self,
        store: &dyn Store,
        alg: &'static Algorithm,
        root: &str,
        edits: &BTreeMap<Path, Edit>,
    ) -> Result<String> {
        let owned = split(edits);
        let pairs: Vec<(&[&str], &Edit)> = owned.iter().map(|(n, e)| (n.as_slice(), *e)).collect();
        Ok(update_dir(store, alg, None, Some(root), &pairs)?.expect("the root is never dropped"))
    }

    fn lookup(&self, store: &dyn Store, root: &str, path: &Path) -> Result<Option<Entry>> {
        Ok(match resolve(store, root, Some(path))? {
            At::Leaf(e) => Some(e),
            At::Nothing | At::Tree(_) => None,
        })
    }

    fn walk<'s>(&self, store: &'s dyn Store, root: &str, prefix: Option<&Path>) -> Walk<'s> {
        match resolve(store, root, prefix) {
            Ok(at) => walk_at(store, prefix, at),
            Err(e) => Box::new(TreeWalk::fail(store, e)),
        }
    }

    fn diff(&self, store: &dyn Store, a: &str, b: &str, prefix: Option<&Path>) -> Result<Vec<Change>> {
        let mut out = Vec::new();
        diff_at(store, prefix, resolve(store, a, prefix)?, resolve(store, b, prefix)?, &mut out)?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cas_core::mem::MemBackend;
    use cas_core::{AllowAll, CoreStore, Expect, Policy, Who};

    fn b3() -> &'static Algorithm {
        cas_core::algo::lookup("blake3").unwrap()
    }

    fn store() -> cas_core::Session {
        CoreStore::new(MemBackend::new(), AllowAll, Policy::permissive()).session(Who::anonymous())
    }

    fn put_raw(s: &dyn Store, text: &str) -> String {
        let key = b3().key(text.as_bytes());
        s.write(&key, text.as_bytes(), &Expect::Absent).unwrap();
        key
    }

    #[test]
    fn empty_root_encoding() {
        let s = store();
        let root = PathTree.build(&s, b3(), &BTreeMap::new()).unwrap();
        assert_eq!(s.read(&root).unwrap().unwrap(), b"cas-tree path/1\n");
    }

    #[test]
    fn encoding_is_readable() {
        let s = store();
        let k = b3().key(b"hi\n");
        let mut m = BTreeMap::new();
        m.insert(Path::new("b c").unwrap(), Entry::new(&k, Mode::Exec).unwrap());
        m.insert(Path::new("a/x").unwrap(), Entry::new(&k, Mode::File).unwrap());
        let root = PathTree.build(&s, b3(), &m).unwrap();
        let text = String::from_utf8(s.read(&root).unwrap().unwrap()).unwrap();
        let sub = b3().key(format!("cas-tree path/1\nfile {k} x\n").as_bytes());
        assert_eq!(text, format!("cas-tree path/1\ntree {sub} a\nexec {k} b c\n"));
    }

    #[test]
    fn rejects_non_canonical_nodes() {
        let s = store();
        let k = b3().key(b"x");
        let leaf = put_raw(&s, &format!("cas-tree path/1\nfile {k} x\n"));
        let empty = put_raw(&s, "cas-tree path/1\n");
        let bad = [
            "cas-tree path/2\n".to_string(),
            "cas-tree path/1".to_string(),
            format!("cas-tree path/1\nfile {k} x"),
            format!("cas-tree path/1\nfile {k} b\nfile {k} a\n"),
            format!("cas-tree path/1\nfile {k} a\nfile {k} a\n"),
            format!("cas-tree path/1\nblob {k} a\n"),
            format!("cas-tree path/1\nfile {} a\n", &k[..20]),
            "cas-tree path/1\nfile main a\n".to_string(),
            format!("cas-tree path/1\nfile {k} .\n"),
            format!("cas-tree path/1\nfile {k} a/b\n"),
            format!("cas-tree path/1\nfile {k}\n"),
            format!("cas-tree path/1\nfile  {k} a\n"),
            format!("cas-tree path/1\n\nfile {k} a\n"),
        ];
        for text in bad {
            let root = put_raw(&s, &text);
            assert!(
                matches!(PathTree.lookup(&s, &root, &Path::new("a").unwrap()), Err(Error::Malformed { .. })),
                "{text:?}"
            );
        }
        // An empty subtree is malformed; the leaf node beside it is fine.
        let root = put_raw(&s, &format!("cas-tree path/1\ntree {empty} a\ntree {leaf} b\n"));
        assert!(matches!(PathTree.lookup(&s, &root, &Path::new("a/x").unwrap()), Err(Error::Malformed { .. })));
        assert_eq!(
            PathTree.lookup(&s, &root, &Path::new("b/x").unwrap()).unwrap(),
            Some(Entry::new(&k, Mode::File).unwrap())
        );
        assert!(PathTree.walk(&s, &root, None).any(|r| r.is_err()));
    }

    #[test]
    fn missing_subtree_is_reported() {
        let s = store();
        let gone = b3().key(b"never written");
        let root = put_raw(&s, &format!("cas-tree path/1\ntree {gone} a\n"));
        assert_eq!(PathTree.lookup(&s, &root, &Path::new("a/b").unwrap()), Err(Error::Missing(gone.clone())));
        let walked: Vec<_> = PathTree.walk(&s, &root, None).collect();
        assert_eq!(walked, vec![Err(Error::Missing(gone))]);
    }
}
