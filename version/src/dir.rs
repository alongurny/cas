//! Directory formats. `dir` (spec/formats/dir.md): text lines of
//! `<format> <key> <name>`, names strictly ascending, the empty blob the
//! empty directory. `tree` (spec/formats/tree.md): the same entries in
//! binary, cut into blobs by a hash of each name so that a big directory
//! shares its unchanged parts between versions.

use crate::{Result, View, is_blob_key, malformed, valid_name};
use cas_core::Store;
use cas_core::algo::{self, Algorithm};
use std::collections::BTreeSet;

/// One entry of a directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub name: String,
    pub child: View,
}

/// A directory format.
pub enum Dir {
    Text,
    Tree,
}

pub static DIR: Dir = Dir::Text;
pub static TREE: Dir = Dir::Tree;

/// What one `tree` blob holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Blob {
    /// Entries of the directory.
    Items(Vec<Item>),
    /// Blobs one level down, each by the greatest name under it.
    Parts { level: u64, parts: Vec<(String, String)> },
}

/// Bits of the name hash per level: blobs hold 64 entries on average.
const BITS: u32 = 6;

/// How many levels a name ends a blob at (spec/formats/tree.md).
pub fn height(name: &str) -> u32 {
    let h = blake3_alg().hash(name.as_bytes());
    u64::from_le_bytes(h[..8].try_into().unwrap()).trailing_zeros() / BITS
}

fn blake3_alg() -> &'static Algorithm {
    algo::lookup("blake3").unwrap()
}

fn tree_format() -> &'static str {
    crate::by_alias("tree").unwrap().id()
}

impl Dir {
    /// Check one blob of this format on its own.
    pub fn check(&self, key: &str, bytes: &[u8]) -> Result<()> {
        match self {
            Dir::Text => Dir::text_decode(key, bytes).map(|_| ()),
            Dir::Tree => tree_decode(bytes).map(|_| ()).map_err(|e| malformed(key, e)),
        }
    }

    /// The views inside one blob: the entries, or a `tree` blob's parts.
    pub fn blob_children(&self, key: &str, bytes: &[u8]) -> Result<Vec<View>> {
        match self {
            Dir::Text => Ok(Dir::text_decode(key, bytes)?.into_iter().map(|i| i.child).collect()),
            Dir::Tree => Ok(match tree_decode(bytes).map_err(|e| malformed(key, e))? {
                Blob::Items(items) => items.into_iter().map(|i| i.child).collect(),
                Blob::Parts { parts, .. } => {
                    parts.into_iter().map(|(_, k)| View { format: tree_format().to_string(), key: k }).collect()
                }
            }),
        }
    }

    /// The blobs of a directory holding `items` (sorted, valid names), each
    /// before any blob that names it, and the key of its top blob.
    pub fn blobs(&self, alg: &'static Algorithm, items: &[Item]) -> (String, Vec<(String, Vec<u8>)>) {
        match self {
            Dir::Text => {
                let bytes = Dir::text_encode(items);
                (alg.key(&bytes), vec![(alg.key(&bytes), bytes)])
            }
            Dir::Tree => tree_blobs(alg, items),
        }
    }

    /// Every entry of the directory whose top blob is `bytes` (at `key`),
    /// reading the blobs below it and checking that they fit together.
    pub fn load(&self, store: &dyn Store, key: &str, bytes: &[u8]) -> Result<Vec<Item>> {
        match self {
            Dir::Text => Dir::text_decode(key, bytes),
            Dir::Tree => tree_load(store, key, bytes),
        }
    }

    /// The entry `name`, reading only the blobs on the way to it.
    pub fn find(&self, store: &dyn Store, key: &str, bytes: &[u8], name: &str) -> Result<Option<View>> {
        match self {
            Dir::Text => Dir::text_find(key, bytes, name),
            Dir::Tree => tree_find(store, key, bytes, name),
        }
    }
}

/// Answers of [`Dir::find_memo`] by (name, blob key): the blob's level and
/// greatest name (to check it against any parent naming it), and the entry.
pub type FindMemo = std::collections::HashMap<(String, String), (u64, Option<String>, Option<View>)>;

impl Dir {
    /// [`Dir::find`] that remembers its answer for every blob it reads, so
    /// looking `name` up in many versions reads only the blobs that differ.
    pub fn find_memo(&self, store: &dyn Store, key: &str, name: &str, memo: &mut FindMemo) -> Result<Option<View>> {
        match self {
            Dir::Text => {
                if let Some((_, _, v)) = memo.get(&(name.to_string(), key.to_string())) {
                    return Ok(v.clone());
                }
                let v = Dir::text_find(key, &crate::read_blob(store, key)?, name)?;
                memo.insert((name.to_string(), key.to_string()), (0, None, v.clone()));
                Ok(v)
            }
            Dir::Tree => Ok(tree_find_memo(store, key, name, memo)?.2),
        }
    }
}

fn tree_find_memo(
    store: &dyn Store,
    key: &str,
    name: &str,
    memo: &mut FindMemo,
) -> Result<(u64, Option<String>, Option<View>)> {
    let id = (name.to_string(), key.to_string());
    if let Some(a) = memo.get(&id) {
        return Ok(a.clone());
    }
    let bytes = crate::read_blob(store, key)?;
    let (level, last, hit) = scan(&bytes, name).map_err(|e| malformed(key, e))?;
    let a = if level == 0 {
        (0, last, hit)
    } else {
        let found = match hit {
            None => None,
            Some(part) => {
                let (l, got, v) = tree_find_memo(store, part.key(), name, memo)?;
                if l + 1 != level || got.as_deref() != Some(part.format()) {
                    return Err(malformed(part.key(), "a blob is not where its parent says"));
                }
                v
            }
        };
        (level, last, found)
    };
    memo.insert(id, a.clone());
    Ok(a)
}

/// One pass over a `tree` blob for `name`, allocating only the answer: its
/// level, its greatest name, and the entry `name` (in a leaf) or the part
/// that would hold it (in an inner blob, as a view whose format is the
/// part's greatest name). For lookups in stored, hash-checked blobs: it
/// checks bounds and name order, not every canonical-form rule
/// [`tree_decode`] checks.
fn scan(b: &[u8], name: &str) -> std::result::Result<(u64, Option<String>, Option<View>), String> {
    if b.is_empty() {
        return Ok((0, None, None));
    }
    let mut r = Reader { b, at: 0 };
    let level = r.varint()?;
    let algs = get_algs(&mut r)?;
    let mut used = vec![false; algs.len()];
    let formats: Vec<String> = if level == 0 {
        let nf = r.count()?;
        (0..nf).map(|_| get_key(&mut r, &algs, &mut used)).collect::<std::result::Result<_, _>>()?
    } else {
        Vec::new()
    };
    let n = r.count()?;
    if n == 0 {
        return Err("no entries".into());
    }
    let (mut cur, mut prev) = (Vec::<u8>::new(), Vec::<u8>::new());
    let mut hit = None;
    for i in 0..n {
        std::mem::swap(&mut cur, &mut prev);
        let shared = r.varint()? as usize;
        let len = r.count()?;
        let rest = r.take(len)?;
        cur.clear();
        cur.extend_from_slice(prev.get(..shared).ok_or("shared prefix too long")?);
        cur.extend_from_slice(rest);
        if i > 0 && cur <= prev {
            return Err("names out of order".into());
        }
        let fi = if level == 0 { Some(r.varint()? as usize) } else { None };
        let key_at = r.at;
        let a = *algs.get(r.varint()? as usize).ok_or("algorithm index out of range")?;
        r.take(a.digest_len)?;
        let wanted = if level == 0 {
            cur.as_slice() == name.as_bytes()
        } else {
            hit.is_none() && cur.as_slice() >= name.as_bytes()
        };
        if wanted {
            let mut sub = Reader { b, at: key_at };
            let key = get_key(&mut sub, &algs, &mut used)?;
            let format = match fi {
                Some(fi) => formats.get(fi).ok_or("format index out of range")?.clone(),
                None => String::from_utf8(cur.clone()).map_err(|_| "name is not UTF-8")?,
            };
            hit = Some(View { format, key });
        }
    }
    if r.at != b.len() {
        return Err("trailing bytes".into());
    }
    let last = String::from_utf8(cur).map_err(|_| "name is not UTF-8")?;
    Ok((level, Some(last), hit))
}

// The `tree` format.

fn varint(out: &mut Vec<u8>, mut n: u64) {
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn byte(&mut self) -> std::result::Result<u8, String> {
        let b = *self.b.get(self.at).ok_or("truncated")?;
        self.at += 1;
        Ok(b)
    }
    fn varint(&mut self) -> std::result::Result<u64, String> {
        let mut n = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.byte()?;
            n |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                // The shortest form only: a last byte of 0 adds nothing.
                if b == 0 && shift > 0 {
                    return Err("not the canonical encoding (varint)".into());
                }
                return Ok(n);
            }
        }
        Err("varint too long".into())
    }
    fn take(&mut self, n: usize) -> std::result::Result<&'a [u8], String> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.b.len()).ok_or("truncated")?;
        let s = &self.b[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn count(&mut self) -> std::result::Result<usize, String> {
        let n = self.varint()?;
        // Each counted thing takes at least a byte.
        if n as usize > self.b.len() - self.at {
            return Err("count past the end".into());
        }
        Ok(n as usize)
    }
}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// The algorithm table of a blob: the names of the algorithms of its keys.
fn alg_table<'k>(keys: impl Iterator<Item = &'k str>) -> Vec<&'k str> {
    let set: BTreeSet<&str> = keys.map(|k| k.split_once(':').unwrap().0).collect();
    set.into_iter().collect()
}

fn put_key(out: &mut Vec<u8>, algs: &[&str], key: &str) {
    let (alg, hex) = key.split_once(':').unwrap();
    varint(out, algs.binary_search(&alg).unwrap() as u64);
    let h = hex.as_bytes();
    let val = |c: u8| (c as char).to_digit(16).unwrap() as u8;
    out.extend(h.chunks(2).map(|p| (val(p[0]) << 4) | val(p[1])));
}

fn get_key(r: &mut Reader, algs: &[&'static Algorithm], used: &mut [bool]) -> std::result::Result<String, String> {
    let i = r.varint()? as usize;
    let a = *algs.get(i).ok_or("algorithm index out of range")?;
    used[i] = true;
    let d = r.take(a.digest_len)?;
    let mut s = String::with_capacity(a.name.len() + 1 + 2 * d.len());
    s.push_str(a.name);
    s.push(':');
    for b in d {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 15) as usize] as char);
    }
    Ok(s)
}

fn put_name(out: &mut Vec<u8>, prev: &str, name: &str) {
    let shared = prev.bytes().zip(name.bytes()).take_while(|(a, b)| a == b).count();
    varint(out, shared as u64);
    varint(out, (name.len() - shared) as u64);
    out.extend_from_slice(&name.as_bytes()[shared..]);
}

fn get_name(r: &mut Reader, prev: &str) -> std::result::Result<String, String> {
    let shared = r.varint()? as usize;
    let n = r.count()?;
    let rest = r.take(n)?;
    let p = prev.as_bytes();
    let head = p.get(..shared).ok_or("shared prefix too long")?;
    // The shared prefix is the longest one.
    if let (Some(a), Some(b)) = (p.get(shared), rest.first())
        && a == b
    {
        return Err("not the canonical encoding (shared prefix)".into());
    }
    let mut v = Vec::with_capacity(shared + n);
    v.extend_from_slice(head);
    v.extend_from_slice(rest);
    String::from_utf8(v).map_err(|_| "name is not UTF-8".into())
}

fn put_algs(out: &mut Vec<u8>, algs: &[&str]) {
    varint(out, algs.len() as u64);
    for a in algs {
        varint(out, a.len() as u64);
        out.extend_from_slice(a.as_bytes());
    }
}

fn get_algs(r: &mut Reader) -> std::result::Result<Vec<&'static Algorithm>, String> {
    let n = r.count()?;
    (0..n)
        .map(|_| {
            let len = r.count()?;
            let name = std::str::from_utf8(r.take(len)?).map_err(|_| "algorithm name is not UTF-8")?;
            algo::lookup(name).ok_or_else(|| format!("unknown algorithm {name:?}"))
        })
        .collect()
}

/// The bytes of one blob. Only the whole empty directory is empty.
pub fn tree_encode(blob: &Blob) -> Vec<u8> {
    let mut out = Vec::new();
    match blob {
        Blob::Items(items) if items.is_empty() => {}
        Blob::Items(items) => {
            out.push(0);
            let algs = alg_table(items.iter().flat_map(|i| [i.child.format(), i.child.key()]));
            put_algs(&mut out, &algs);
            let formats: Vec<&str> =
                items.iter().map(|i| i.child.format()).collect::<BTreeSet<_>>().into_iter().collect();
            varint(&mut out, formats.len() as u64);
            for f in &formats {
                put_key(&mut out, &algs, f);
            }
            varint(&mut out, items.len() as u64);
            let mut prev = "";
            for i in items {
                put_name(&mut out, prev, &i.name);
                varint(&mut out, formats.binary_search(&i.child.format()).unwrap() as u64);
                put_key(&mut out, &algs, i.child.key());
                prev = &i.name;
            }
        }
        Blob::Parts { level, parts } => {
            varint(&mut out, *level);
            let algs = alg_table(parts.iter().map(|(_, k)| k.as_str()));
            put_algs(&mut out, &algs);
            varint(&mut out, parts.len() as u64);
            let mut prev = "";
            for (name, key) in parts {
                put_name(&mut out, prev, name);
                put_key(&mut out, &algs, key);
                prev = name;
            }
        }
    }
    out
}

/// One blob, checked on its own: valid names in strictly ascending order,
/// valid keys, chunk ends only where the rules allow (the last name may or
/// may not be one: that depends on the blobs beside it), and the canonical
/// encoding.
pub fn tree_decode(b: &[u8]) -> std::result::Result<Blob, String> {
    decode(b, true)
}

/// [`tree_decode`] without checking where the blob is cut, the one check
/// that hashes every name: for reads whose answer only guides a choice (gc's
/// delta hints), not what is kept.
pub fn tree_decode_uncut(b: &[u8]) -> std::result::Result<Blob, String> {
    decode(b, false)
}

fn decode(b: &[u8], check_cut: bool) -> std::result::Result<Blob, String> {
    if b.is_empty() {
        return Ok(Blob::Items(Vec::new()));
    }
    let mut r = Reader { b, at: 0 };
    let level = r.varint()?;
    let algs = get_algs(&mut r)?;
    if algs.windows(2).any(|w| w[0].name >= w[1].name) {
        return Err("not the canonical encoding (algorithm table)".into());
    }
    let mut used = vec![false; algs.len()];
    let blob = if level == 0 {
        let nf = r.count()?;
        let formats: Vec<String> =
            (0..nf).map(|_| get_key(&mut r, &algs, &mut used)).collect::<std::result::Result<_, _>>()?;
        if formats.windows(2).any(|w| w[0] >= w[1]) {
            return Err("not the canonical encoding (format table)".into());
        }
        let mut fused = vec![false; formats.len()];
        let n = r.count()?;
        let mut items: Vec<Item> = Vec::with_capacity(n);
        for _ in 0..n {
            let name = get_name(&mut r, items.last().map_or("", |i| &i.name))?;
            let fi = r.varint()? as usize;
            let f = formats.get(fi).ok_or("format index out of range")?.clone();
            fused[fi] = true;
            let key = get_key(&mut r, &algs, &mut used)?;
            items.push(Item { name, child: View { format: f, key } });
        }
        if items.is_empty() {
            return Err("an empty leaf is the empty blob".into());
        }
        if fused.contains(&false) {
            return Err("not the canonical encoding (unused format)".into());
        }
        Blob::Items(items)
    } else {
        let n = r.count()?;
        let mut parts: Vec<(String, String)> = Vec::with_capacity(n);
        for _ in 0..n {
            let name = get_name(&mut r, parts.last().map_or("", |p| &p.0))?;
            let key = get_key(&mut r, &algs, &mut used)?;
            parts.push((name, key));
        }
        if parts.is_empty() {
            return Err("an inner blob has parts".into());
        }
        Blob::Parts { level, parts }
    };
    if r.at != b.len() {
        return Err("trailing bytes".into());
    }
    let names: Vec<&str> = match &blob {
        Blob::Items(items) => items.iter().map(|i| i.name.as_str()).collect(),
        Blob::Parts { parts, .. } => parts.iter().map(|p| p.0.as_str()).collect(),
    };
    if let Some(n) = names.iter().find(|n| !valid_name(n)) {
        return Err(format!("bad name {n:?}"));
    }
    if names.windows(2).any(|w| w[0] >= w[1]) {
        return Err("names out of order".into());
    }
    if check_cut && names[..names.len() - 1].iter().any(|n| u64::from(height(n)) > level) {
        return Err(format!("a blob at level {level} is not cut by name"));
    }
    if used.contains(&false) {
        return Err("not the canonical encoding (unused algorithm)".into());
    }
    // With the shortest varints, the longest shared prefixes, sorted and
    // used tables and sorted names, these bytes are what `tree_encode`
    // writes: checked in the tests rather than by encoding again here.
    debug_assert!(!check_cut || tree_encode(&blob) == b);
    Ok(blob)
}

/// Cut `items` (sorted by name) after each name higher than `level`.
fn cut<T>(items: Vec<T>, name: impl Fn(&T) -> &str, level: u64) -> Vec<Vec<T>> {
    let mut out = Vec::new();
    let mut cur = Vec::new();
    for it in items {
        let end = u64::from(height(name(&it))) > level;
        cur.push(it);
        if end {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn tree_blobs(alg: &'static Algorithm, items: &[Item]) -> (String, Vec<(String, Vec<u8>)>) {
    let mut out = Vec::new();
    let emit = |blob: Blob, out: &mut Vec<(String, Vec<u8>)>| {
        let bytes = tree_encode(&blob);
        let key = alg.key(&bytes);
        out.push((key.clone(), bytes));
        key
    };
    if items.is_empty() {
        let k = emit(Blob::Items(Vec::new()), &mut out);
        return (k, out);
    }
    let mut list: Vec<(String, String)> = cut(items.to_vec(), |i: &Item| &i.name, 0)
        .into_iter()
        .map(|c| {
            let last = c.last().unwrap().name.clone();
            (last, emit(Blob::Items(c), &mut out))
        })
        .collect();
    let mut level = 1;
    while list.len() > 1 {
        list = cut(list, |p: &(String, String)| &p.0, level)
            .into_iter()
            .map(|c| {
                let last = c.last().unwrap().0.clone();
                (last, emit(Blob::Parts { level, parts: c }, &mut out))
            })
            .collect();
        level += 1;
    }
    (list.pop().unwrap().1, out)
}

fn read_tree_blob(store: &dyn Store, key: &str) -> Result<Blob> {
    let bytes = crate::read_blob(store, key)?;
    tree_decode(&bytes).map_err(|e| malformed(key, e))
}

/// Inner blob keys, and leaves as (greatest name, key).
pub(crate) type Parts = (Vec<String>, Vec<(String, String)>);

/// A `tree` directory's blobs below its top `top`: the inner ones, and the
/// leaves, in name order. Reads the inner blobs only; `checked` decodes
/// them with every check, else without the chunk check.
pub(crate) fn tree_parts(store: &dyn Store, top: &str, checked: bool) -> Result<Parts> {
    let (mut inner, mut leaves) = (Vec::new(), Vec::new());
    // (greatest name, key, known to be a leaf): the parts of a level 1 blob
    // are leaves.
    let mut todo = vec![(String::new(), top.to_string(), false)];
    while let Some((last, key, leaf)) = todo.pop() {
        if leaf {
            leaves.push((last, key));
            continue;
        }
        let bytes = crate::read_blob(store, &key)?;
        let blob = if checked { tree_decode(&bytes) } else { tree_decode_uncut(&bytes) };
        match blob.map_err(|e| malformed(&key, e))? {
            Blob::Items(_) => leaves.push((last, key)),
            Blob::Parts { level, parts } => {
                if key != top {
                    inner.push(key);
                }
                todo.extend(parts.into_iter().rev().map(|(n, k)| (n, k, level == 1)));
            }
        }
    }
    Ok((inner, leaves))
}

/// The entries of directories `a` and `b`, leaving out those in blobs both
/// share: what is left holds every difference between them. Two `tree`
/// directories read only their inner blobs and the leaves that differ.
pub fn load_differing(store: &dyn Store, a: &View, b: &View) -> Result<(Vec<Item>, Vec<Item>)> {
    let is_tree = |v: &View| v.format() == tree_format();
    if !(is_tree(a) && is_tree(b)) {
        return Ok((crate::tree::load(store, a)?, crate::tree::load(store, b)?));
    }
    let (_, al) = tree_parts(store, a.key(), true)?;
    let (_, bl) = tree_parts(store, b.key(), true)?;
    let keys = |ls: &[(String, String)]| ls.iter().map(|l| l.1.clone()).collect::<BTreeSet<String>>();
    let (ak, bk) = (keys(&al), keys(&bl));
    let items = |ls: &[(String, String)], other: &BTreeSet<String>| -> Result<Vec<Item>> {
        let want: Vec<String> = ls.iter().filter(|l| !other.contains(&l.1)).map(|l| l.1.clone()).collect();
        let mut out = Vec::new();
        for (k, r) in want.iter().zip(store.read_many(&want)) {
            let b = r?.ok_or_else(|| crate::Error::Missing(k.clone()))?;
            match tree_decode(&b).map_err(|e| malformed(k, e))? {
                Blob::Items(i) => out.extend(i),
                Blob::Parts { .. } => return Err(malformed(k, "a leaf where a leaf belongs is not one")),
            }
        }
        Ok(out)
    };
    Ok((items(&al, &bk)?, items(&bl, &ak)?))
}

fn tree_load(store: &dyn Store, key: &str, bytes: &[u8]) -> Result<Vec<Item>> {
    let top = tree_decode(bytes).map_err(|e| malformed(key, e))?;
    let (level, parts) = match top {
        Blob::Items(items) => return Ok(items),
        Blob::Parts { level, parts } => (level, parts),
    };
    if parts.len() < 2 {
        return Err(malformed(key, "the top blob has one part"));
    }
    // One level at a time: the blobs of a level are read in one batch.
    let mut level_parts: Vec<(String, String)> = parts;
    let mut level = level;
    loop {
        let keys: Vec<String> = level_parts.iter().map(|(_, k)| k.clone()).collect();
        let mut blobs = Vec::with_capacity(keys.len());
        for (k, r) in keys.iter().zip(store.read_many(&keys)) {
            let b = r?.ok_or_else(|| crate::Error::Missing(k.clone()))?;
            blobs.push(tree_decode(&b).map_err(|e| malformed(k, e))?);
        }
        let n = blobs.len();
        let mut next_parts = Vec::new();
        let mut items = Vec::new();
        let mut prev: Option<&str> = None;
        for (i, ((last, k), blob)) in level_parts.iter().zip(&blobs).enumerate() {
            let (blob_level, names): (u64, Vec<&str>) = match blob {
                Blob::Items(it) => (0, it.iter().map(|x| x.name.as_str()).collect()),
                Blob::Parts { level, parts } => (*level, parts.iter().map(|p| p.0.as_str()).collect()),
            };
            let fits = blob_level + 1 == level
                && !names.is_empty()
                && names.last() == Some(&last.as_str())
                && prev.is_none_or(|p| names[0] > p)
                && (i + 1 == n || u64::from(height(last)) > blob_level);
            if !fits {
                return Err(malformed(k, "a blob is not where its parent says"));
            }
            prev = Some(last);
        }
        for blob in blobs {
            match blob {
                Blob::Items(it) => items.extend(it),
                Blob::Parts { parts, .. } => next_parts.extend(parts),
            }
        }
        if level == 1 {
            return Ok(items);
        }
        level_parts = next_parts;
        level -= 1;
    }
}

fn tree_find(store: &dyn Store, key: &str, bytes: &[u8], name: &str) -> Result<Option<View>> {
    let mut blob = tree_decode(bytes).map_err(|e| malformed(key, e))?;
    loop {
        match blob {
            Blob::Items(items) => {
                return Ok(items.binary_search_by(|i| i.name.as_str().cmp(name)).ok().map(|i| items[i].child.clone()));
            }
            Blob::Parts { level, parts } => {
                let i = parts.partition_point(|(last, _)| last.as_str() < name);
                let Some((last, k)) = parts.get(i) else { return Ok(None) };
                let next = read_tree_blob(store, k)?;
                let ok = match &next {
                    Blob::Items(it) => level == 1 && it.last().is_some_and(|x| &x.name == last),
                    Blob::Parts { level: l, parts: p } => *l + 1 == level && p.last().is_some_and(|x| &x.0 == last),
                };
                if !ok {
                    return Err(malformed(k, "a blob is not where its parent says"));
                }
                blob = next;
            }
        }
    }
}

impl Dir {
    // The `dir` format: text lines.

    fn text_encode(items: &[Item]) -> Vec<u8> {
        let mut out = String::new();
        for it in items {
            out.push_str(&format!("{} {}\n", it.child, it.name));
        }
        out.into_bytes()
    }

    /// The entry `name` of a directory, without building the others: one
    /// pass that checks every line as [`Dir::text_decode`] does but allocates
    /// only the answer.
    fn text_find(key: &str, bytes: &[u8], name: &str) -> Result<Option<View>> {
        let text = std::str::from_utf8(bytes).map_err(|_| malformed(key, "not UTF-8"))?;
        let mut prev: Option<&str> = None;
        let mut prev_format: Option<&str> = None;
        let mut found = None;
        for line in text.split_inclusive('\n') {
            let line = line.strip_suffix('\n').ok_or_else(|| malformed(key, "last line has no newline"))?;
            let mut parts = line.splitn(3, ' ');
            let (Some(f), Some(k), Some(n)) = (parts.next(), parts.next(), parts.next()) else {
                return Err(malformed(key, format!("bad line {line:?}")));
            };
            // Directories hold few formats: a format seen on the line before
            // is already checked.
            if !(prev_format == Some(f) || is_blob_key(f)) || !is_blob_key(k) {
                return Err(malformed(key, format!("bad view {f:?} {k:?}")));
            }
            if !valid_name(n) {
                return Err(malformed(key, format!("bad name {n:?}")));
            }
            if prev.is_some_and(|p| p >= n) {
                return Err(malformed(key, format!("{n:?} out of order")));
            }
            prev = Some(n);
            prev_format = Some(f);
            if n == name {
                found = Some(View { format: f.to_string(), key: k.to_string() });
            }
        }
        Ok(found)
    }

    /// Parse a directory, accepting only the canonical encoding.
    fn text_decode(key: &str, bytes: &[u8]) -> Result<Vec<Item>> {
        let text = std::str::from_utf8(bytes).map_err(|_| malformed(key, "not UTF-8"))?;
        let mut items: Vec<Item> = Vec::new();
        for line in text.split_inclusive('\n') {
            let line = line.strip_suffix('\n').ok_or_else(|| malformed(key, "last line has no newline"))?;
            let mut parts = line.splitn(3, ' ');
            let (Some(f), Some(k), Some(name)) = (parts.next(), parts.next(), parts.next()) else {
                return Err(malformed(key, format!("bad line {line:?}")));
            };
            // Directories hold few formats: a format seen on the line before
            // is already checked.
            let same_format = items.last().is_some_and(|p| p.child.format == f);
            if !(same_format || is_blob_key(f)) || !is_blob_key(k) {
                return Err(malformed(key, format!("bad view {f:?} {k:?}")));
            }
            if !valid_name(name) {
                return Err(malformed(key, format!("bad name {name:?}")));
            }
            if items.last().is_some_and(|prev| prev.name.as_str() >= name) {
                return Err(malformed(key, format!("{name:?} out of order")));
            }
            items.push(Item { name: name.to_string(), child: View { format: f.to_string(), key: k.to_string() } });
        }
        Ok(items)
    }
}
