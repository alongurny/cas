//! Git's own objects as cas formats (spec/formats/git.md): `git-blob`,
//! `git-tree`, `git-commit` and `git-tag` hold a git object exactly as git
//! hashes it (`<type> <n>\0<body>`), so its `sha1` key is its git id. Packs
//! carry them in and out; [`to_git`] and [`from_git`] convert between them
//! and cas histories, the default, in either direction, and `git-links`
//! records which commit a node is, so a conversion is the same both ways.

use crate::history::{History, NodeId, predecessors_first};
use crate::uni::Value;
use crate::{Error, Result, View, by_alias, malformed, read_blob};
use cas_core::algo::{self, Algorithm};
use cas_core::{Expect, Store};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::{Read, Write};

/// The git types and their formats.
pub const TYPES: [(&str, &str, u8); 4] =
    [("blob", "git-blob", 3), ("tree", "git-tree", 2), ("commit", "git-commit", 1), ("tag", "git-tag", 4)];

pub fn sha1() -> &'static Algorithm {
    algo::lookup("sha1").unwrap()
}

/// `<type> <n>\0<body>`.
pub fn loose(kind: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!("{kind} {}\0", body.len()).into_bytes();
    out.extend_from_slice(body);
    out
}

/// The type and body of a git object, when its header is canonical.
pub fn split(b: &[u8]) -> Option<(&'static str, &[u8])> {
    let sp = b.iter().position(|&c| c == b' ')?;
    let kind = TYPES.iter().find(|t| t.0.as_bytes() == &b[..sp])?.0;
    let rest = &b[sp + 1..];
    let nul = rest.iter().position(|&c| c == 0)?;
    let n = std::str::from_utf8(&rest[..nul]).ok()?;
    let canonical = n == "0" || (!n.starts_with('0') && !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()));
    let body = &rest[nul + 1..];
    (canonical && n.parse::<usize>().ok()? == body.len()).then_some((kind, body))
}

/// The format alias of git type `kind`.
pub fn alias(kind: &str) -> Option<&'static str> {
    TYPES.iter().find(|t| t.0 == kind).map(|t| t.1)
}

/// The git type of format `alias`.
pub fn kind_of(alias: &str) -> Option<&'static str> {
    TYPES.iter().find(|t| t.1 == alias).map(|t| t.0)
}

fn hex(id: &[u8]) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

/// The view of git object `id` (40 hex digits) of type `kind`.
pub fn view(kind: &str, id: &str) -> View {
    let f = by_alias(alias(kind).unwrap()).unwrap();
    View::new(f.id(), format!("sha1:{id}")).unwrap()
}

/// The git id in a view's key.
pub fn id_of(v: &View) -> Option<&str> {
    v.key().strip_prefix("sha1:").filter(|h| h.len() == 40)
}

/// One entry of a git tree: mode, name, id.
pub type Entry<'a> = (&'a str, &'a [u8], [u8; 20]);

pub fn tree_entries(body: &[u8]) -> std::result::Result<Vec<Entry<'_>>, String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < body.len() {
        let sp = body[i..].iter().position(|&c| c == b' ').ok_or("an entry without a mode")? + i;
        let mode = std::str::from_utf8(&body[i..sp]).map_err(|_| "a mode is not ASCII")?;
        if mode.is_empty() || !mode.bytes().all(|c| c.is_ascii_digit()) {
            return Err(format!("bad mode {mode:?}"));
        }
        let nul = body[sp + 1..].iter().position(|&c| c == 0).ok_or("an entry without a name")? + sp + 1;
        let name = &body[sp + 1..nul];
        if name.is_empty() || name.contains(&b'/') {
            return Err("a bad entry name".into());
        }
        let id: [u8; 20] = body.get(nul + 1..nul + 21).ok_or("a cut entry")?.try_into().unwrap();
        out.push((mode, name, id));
        i = nul + 21;
    }
    Ok(out)
}

/// The type an entry's mode names; `None` for a submodule (`160000`).
fn entry_kind(mode: &str) -> Option<&'static str> {
    match mode {
        "40000" | "040000" => Some("tree"),
        "160000" => None,
        _ => Some("blob"),
    }
}

/// Header lines of a commit or tag: `(name, value)` up to the message.
fn headers(body: &[u8]) -> Vec<(&str, &str)> {
    let text = match std::str::from_utf8(body) {
        Ok(t) => t,
        // A message in another encoding: the headers are still ASCII.
        Err(e) => std::str::from_utf8(&body[..e.valid_up_to()]).unwrap_or(""),
    };
    let mut out = Vec::new();
    for line in text.split('\n') {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(' ') {
            out.push((k, v));
        }
    }
    out
}

fn is_hex40(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn check_as(b: &[u8], want: &str) -> std::result::Result<(), String> {
    let (kind, body) = split(b).ok_or("not <type> <n>\\0<body>")?;
    if kind != want {
        return Err(format!("a git {kind}, not a {want}"));
    }
    match kind {
        "tree" => tree_entries(body).map(|_| ()),
        "commit" => match headers(body).first() {
            Some(("tree", t)) if is_hex40(t) => Ok(()),
            _ => Err("a commit starts with its tree".into()),
        },
        "tag" => match headers(body).first() {
            Some(("object", t)) if is_hex40(t) => Ok(()),
            _ => Err("a tag starts with its object".into()),
        },
        _ => Ok(()),
    }
}

pub(crate) fn check_tree(b: &[u8]) -> std::result::Result<(), String> {
    check_as(b, "tree")
}
pub(crate) fn check_commit(b: &[u8]) -> std::result::Result<(), String> {
    check_as(b, "commit")
}
pub(crate) fn check_tag(b: &[u8]) -> std::result::Result<(), String> {
    check_as(b, "tag")
}

/// The objects a git object names: a tree's entries (not submodules), a
/// commit's tree and parents, a tag's object.
pub fn children(bytes: &[u8]) -> Vec<View> {
    let Some((kind, body)) = split(bytes) else { return Vec::new() };
    match kind {
        "tree" => tree_entries(body)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(m, _, id)| Some(view(entry_kind(m)?, &hex(&id))))
            .collect(),
        "commit" => headers(body)
            .into_iter()
            .filter_map(|(k, v)| match k {
                "tree" if is_hex40(v) => Some(view("tree", v)),
                "parent" if is_hex40(v) => Some(view("commit", v)),
                _ => None,
            })
            .collect(),
        "tag" => {
            let h = headers(body);
            let obj = h.iter().find(|(k, _)| *k == "object").map(|x| x.1);
            let ty = h.iter().find(|(k, _)| *k == "type").map(|x| x.1);
            match (obj, ty) {
                (Some(o), Some(t)) if is_hex40(o) && alias(t).is_some() => vec![view(t, o)],
                _ => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

/// Store a git object; its id.
pub fn put(store: &dyn Store, kind: &str, body: &[u8]) -> Result<String> {
    put_loose(store, &loose(kind, body))
}

fn put_loose(store: &dyn Store, bytes: &[u8]) -> Result<String> {
    let key = sha1().key(bytes);
    match store.write(&key, bytes, &Expect::Absent) {
        Ok(()) | Err(cas_core::Error::Conflict) => {}
        Err(e) => return Err(e.into()),
    }
    Ok(key["sha1:".len()..].to_string())
}

/// Git objects written together, so the store can keep them packed, each
/// as a delta on the blob named as its likely base.
pub struct Batch<'a> {
    store: &'a dyn Store,
    puts: Vec<cas_core::store::Put>,
    bytes: usize,
    seen: HashSet<String>,
}

impl<'a> Batch<'a> {
    pub fn new(store: &'a dyn Store) -> Batch<'a> {
        Batch { store, puts: Vec::new(), bytes: 0, seen: HashSet::new() }
    }

    /// Queue the whole loose object `lo`, like blob `like`; its id.
    pub fn loose(&mut self, lo: Vec<u8>, like: Option<String>) -> Result<String> {
        let key = sha1().key(&lo);
        let id = key["sha1:".len()..].to_string();
        if self.seen.insert(key.clone()) {
            self.bytes += lo.len();
            let like = like.filter(|l| *l != key);
            self.puts.push(cas_core::store::Put { like, ..cas_core::store::Put::blob(key, lo) });
            if self.bytes >= 64 << 20 {
                self.flush()?;
            }
        }
        Ok(id)
    }

    /// Queue a `kind` object with `body`.
    pub fn put(&mut self, kind: &str, body: &[u8], like: Option<String>) -> Result<String> {
        self.loose(loose(kind, body), like)
    }

    /// Queue any value under its key in `alg`, like `like`.
    pub fn value(&mut self, key: String, value: Vec<u8>, like: Option<String>) -> Result<()> {
        if self.seen.insert(key.clone()) {
            self.bytes += value.len();
            let like = like.filter(|l| *l != key);
            self.puts.push(cas_core::store::Put { like, ..cas_core::store::Put::blob(key, value) });
            if self.bytes >= 64 << 20 {
                self.flush()?;
            }
        }
        Ok(())
    }

    pub fn flush(&mut self) -> Result<()> {
        let puts = std::mem::take(&mut self.puts);
        self.bytes = 0;
        match self.store.write_many(&puts) {
            Ok(()) => Ok(()),
            Err((i, cas_core::Error::Conflict)) => {
                // Written meanwhile: write the rest one by one.
                for p in &puts[i..] {
                    match self.store.write(&p.key, &p.value, &Expect::Absent) {
                        Ok(()) | Err(cas_core::Error::Conflict) => {}
                        Err(e) => return Err(e.into()),
                    }
                }
                Ok(())
            }
            Err((_, e)) => Err(e.into()),
        }
    }
}

/// Read git object `id`: its type and the whole loose object.
pub fn get(store: &dyn Store, id: &str) -> Result<(&'static str, Vec<u8>)> {
    let key = format!("sha1:{id}");
    let b = read_blob(store, &key)?;
    let kind = split(&b).ok_or_else(|| malformed(&key, "not a git object"))?.0;
    Ok((kind, b))
}

pub fn has(store: &dyn Store, id: &str) -> Result<bool> {
    Ok(store.has_many(&[format!("sha1:{id}")])?[0])
}

// ---- Packs ----

fn bad(m: impl Into<String>) -> Error {
    Error::Malformed { key: "pack".into(), reason: m.into() }
}

/// Apply a git delta to `base`.
fn apply_delta(base: &[u8], d: &[u8]) -> Result<Vec<u8>> {
    let mut i = 0;
    let varint = |i: &mut usize| -> Result<usize> {
        let (mut n, mut shift) = (0usize, 0);
        loop {
            let c = *d.get(*i).ok_or_else(|| bad("cut delta"))?;
            *i += 1;
            n |= ((c & 0x7f) as usize) << shift;
            shift += 7;
            if c & 0x80 == 0 {
                return Ok(n);
            }
        }
    };
    let src = varint(&mut i)?;
    let dst = varint(&mut i)?;
    if src != base.len() {
        return Err(bad("a delta for another base"));
    }
    let mut out = Vec::with_capacity(dst);
    while i < d.len() {
        let op = d[i];
        i += 1;
        if op & 0x80 != 0 {
            let (mut off, mut size) = (0usize, 0usize);
            for k in 0..4 {
                if op & (1 << k) != 0 {
                    off |= (*d.get(i).ok_or_else(|| bad("cut delta"))? as usize) << (8 * k);
                    i += 1;
                }
            }
            for k in 0..3 {
                if op & (0x10 << k) != 0 {
                    size |= (*d.get(i).ok_or_else(|| bad("cut delta"))? as usize) << (8 * k);
                    i += 1;
                }
            }
            if size == 0 {
                size = 0x10000;
            }
            out.extend_from_slice(base.get(off..off + size).ok_or_else(|| bad("a delta copies past its base"))?);
        } else if op != 0 {
            let n = op as usize;
            out.extend_from_slice(d.get(i..i + n).ok_or_else(|| bad("cut delta"))?);
            i += n;
        } else {
            return Err(bad("delta op 0"));
        }
    }
    if out.len() != dst {
        return Err(bad("a delta of the wrong size"));
    }
    Ok(out)
}

fn inflate(data: &[u8], size: usize) -> Result<(Vec<u8>, usize)> {
    let mut z = flate2::Decompress::new(true);
    let mut out = Vec::with_capacity(size);
    loop {
        if out.capacity() - out.len() < 64 {
            out.reserve(out.len().max(1024));
        }
        let before_in = z.total_in();
        let st = z
            .decompress_vec(&data[z.total_in() as usize..], &mut out, flate2::FlushDecompress::None)
            .map_err(|e| bad(format!("zlib: {e}")))?;
        if st == flate2::Status::StreamEnd {
            break;
        }
        if z.total_in() == before_in && out.len() < out.capacity() {
            return Err(bad("cut zlib data"));
        }
    }
    if out.len() != size {
        return Err(bad("an object of the wrong size"));
    }
    Ok((out, z.total_in() as usize))
}

/// Store every object of a pack (version 2 or 3); their views, in pack
/// order. Deltas may name bases earlier in the pack or already stored (a
/// thin pack). The pack's trailing checksum is checked.
pub fn unpack(store: &dyn Store, pack: &[u8]) -> Result<Vec<View>> {
    if pack.len() < 32 || &pack[..4] != b"PACK" {
        return Err(bad("not a pack"));
    }
    let version = u32::from_be_bytes(pack[4..8].try_into().unwrap());
    if version != 2 && version != 3 {
        return Err(bad(format!("pack version {version}")));
    }
    let (body, sum) = pack.split_at(pack.len() - 20);
    if sha1().hash(body) != sum {
        return Err(bad("the pack's checksum is wrong"));
    }
    let count = u32::from_be_bytes(pack[8..12].try_into().unwrap()) as usize;
    // Objects by offset (type, bytes), resolved; deltas waiting on a base.
    let mut at: HashMap<usize, (&'static str, Vec<u8>)> = HashMap::new();
    let mut by_id: HashMap<String, usize> = HashMap::new();
    enum Base {
        Ofs(usize),
        Ref(String),
    }
    let mut waiting: Vec<(usize, Base, Vec<u8>)> = Vec::new();
    let mut order = Vec::with_capacity(count);
    let mut ids: HashMap<usize, String> = HashMap::new();
    let mut batch = Batch::new(store);
    let mut i = 12;
    for _ in 0..count {
        let start = i;
        let mut c = *body.get(i).ok_or_else(|| bad("cut pack"))?;
        i += 1;
        let ty = (c >> 4) & 7;
        let mut size = (c & 15) as usize;
        let mut shift = 4;
        while c & 0x80 != 0 {
            c = *body.get(i).ok_or_else(|| bad("cut pack"))?;
            i += 1;
            size |= ((c & 0x7f) as usize) << shift;
            shift += 7;
        }
        let base = match ty {
            6 => {
                let mut c = *body.get(i).ok_or_else(|| bad("cut pack"))?;
                i += 1;
                let mut off = (c & 0x7f) as usize;
                while c & 0x80 != 0 {
                    c = *body.get(i).ok_or_else(|| bad("cut pack"))?;
                    i += 1;
                    off = ((off + 1) << 7) | (c & 0x7f) as usize;
                }
                Some(Base::Ofs(start.checked_sub(off).ok_or_else(|| bad("a delta before the pack"))?))
            }
            7 => {
                let id = hex(body.get(i..i + 20).ok_or_else(|| bad("cut pack"))?);
                i += 20;
                Some(Base::Ref(id))
            }
            1..=4 => None,
            t => return Err(bad(format!("object type {t}"))),
        };
        let (data, used) = inflate(&body[i..], size)?;
        i += used;
        order.push(start);
        match base {
            None => {
                let kind = TYPES.iter().find(|t| t.2 == ty).unwrap().0;
                let lo = loose(kind, &data);
                let id = batch.loose(lo.clone(), None)?;
                by_id.insert(id.clone(), start);
                ids.insert(start, id);
                at.insert(start, (kind, lo));
            }
            Some(b) => waiting.push((start, b, data)),
        }
    }
    if i != body.len() {
        return Err(bad("bytes after the last object"));
    }
    // Resolve deltas until none is left or none moves.
    while !waiting.is_empty() {
        let before = waiting.len();
        let mut still = Vec::new();
        for (start, b, d) in waiting {
            let base: Option<(&'static str, Vec<u8>, String)> = match &b {
                Base::Ofs(o) => at.get(o).map(|(k, lo)| (*k, lo.clone(), ids[o].clone())),
                Base::Ref(id) => match by_id.get(id) {
                    Some(o) => at.get(o).map(|(k, lo)| (*k, lo.clone(), id.clone())),
                    None if has(store, id)? => {
                        let (k, lo) = get(store, id)?;
                        Some((k, lo, id.clone()))
                    }
                    None => None,
                },
            };
            match base {
                Some((kind, lo, base_id)) => {
                    let body = split(&lo).unwrap().1;
                    let out = loose(kind, &apply_delta(body, &d)?);
                    // Kept as git kept it: a delta on the same base.
                    let id = batch.loose(out.clone(), Some(format!("sha1:{base_id}")))?;
                    by_id.insert(id.clone(), start);
                    ids.insert(start, id);
                    at.insert(start, (kind, out));
                }
                None => still.push((start, b, d)),
            }
        }
        if still.len() == before {
            return Err(bad("a delta whose base is in neither the pack nor the store"));
        }
        waiting = still;
    }
    batch.flush()?;
    Ok(order
        .iter()
        .map(|o| {
            let (kind, lo) = &at[o];
            view(kind, &sha1().key(lo)["sha1:".len()..])
        })
        .collect())
}

/// A pack (version 2) of the git objects `ids`, each whole (no deltas).
pub fn pack(store: &dyn Store, ids: &[String]) -> Result<Vec<u8>> {
    let set: HashSet<&str> = ids.iter().map(String::as_str).collect();
    let mut hints = delta_bases(store, ids, &set)?;
    limit_depth(&mut hints, MAX_DEPTH);
    let mut out = Vec::new();
    out.extend_from_slice(b"PACK");
    out.extend_from_slice(&2u32.to_be_bytes());
    out.extend_from_slice(&(ids.len() as u32).to_be_bytes());
    // Bases first, so each delta can name its base by offset.
    let mut order: Vec<&str> = Vec::with_capacity(ids.len());
    let mut placed: HashSet<&str> = HashSet::new();
    for id in ids {
        let mut chain = vec![id.as_str()];
        while let Some(b) = hints.get(*chain.last().unwrap()) {
            if placed.contains(b.as_str()) || chain.contains(&b.as_str()) {
                break;
            }
            chain.push(b.as_str());
        }
        for c in chain.into_iter().rev() {
            if placed.insert(c) {
                order.push(c);
            }
        }
    }
    // Each object made ready on every core: its base, if a delta, and its
    // deflated data; then laid out in order, bases by offset.
    let ready = parallel(&order, |id| -> Result<(Option<&str>, u8, usize, Vec<u8>)> {
        let (kind, lo) = get(store, id)?;
        let body = split(&lo).unwrap().1;
        if let Some(b) = hints.get(*id) {
            let (_, blo) = get(store, b)?;
            if let Some(d) = delta(split(&blo).unwrap().1, body)
                && d.len() < body.len() / 2
            {
                return Ok((Some(b.as_str()), 6, d.len(), deflate(&d)?));
            }
        }
        let ty = TYPES.iter().find(|t| t.0 == kind).unwrap().2;
        Ok((None, ty, body.len(), deflate(body)?))
    })?;
    let mut offsets: HashMap<&str, usize> = HashMap::new();
    for (id, (base, ty, len, z)) in order.iter().zip(ready) {
        let start = out.len();
        obj_header(&mut out, ty, len);
        if let Some(b) = base {
            let mut off = start - offsets[b];
            let mut enc = vec![(off & 0x7f) as u8];
            off >>= 7;
            while off > 0 {
                off -= 1;
                enc.push(0x80 | (off & 0x7f) as u8);
                off >>= 7;
            }
            enc.reverse();
            out.extend_from_slice(&enc);
        }
        out.extend_from_slice(&z);
        offsets.insert(id, start);
    }
    let sum = sha1().hash(&out);
    out.extend_from_slice(&sum);
    Ok(out)
}

/// How long a chain of deltas may grow, as git's default.
const MAX_DEPTH: usize = 50;

fn obj_header(out: &mut Vec<u8>, ty: u8, len: usize) {
    let mut n = len;
    let mut c = (ty << 4) | (n & 15) as u8;
    n >>= 4;
    while n > 0 {
        out.push(c | 0x80);
        c = (n & 0x7f) as u8;
        n >>= 7;
    }
    out.push(c);
}

fn deflate(data: &[u8]) -> Result<Vec<u8>> {
    let mut z = flate2::write::ZlibEncoder::new(Vec::with_capacity(data.len() / 2), flate2::Compression::default());
    z.write_all(data)?;
    Ok(z.finish()?)
}

/// `f` of each item, on every core, in order.
fn parallel<T: Sync, U: Send>(items: &[T], f: impl Fn(&T) -> Result<U> + Sync) -> Result<Vec<U>> {
    let threads = cas_core::threads::get().min(items.len()).max(1);
    if threads == 1 {
        return items.iter().map(f).collect();
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let parts: Vec<Vec<(usize, Result<U>)>> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if i >= items.len() {
                            return mine;
                        }
                        mine.push((i, f(&items[i])));
                    }
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut out: Vec<Option<U>> = (0..items.len()).map(|_| None).collect();
    for (i, r) in parts.into_iter().flatten() {
        out[i] = Some(r?);
    }
    Ok(out.into_iter().map(Option::unwrap).collect())
}

/// A likely delta base in `set` for objects of `ids`: each tree and blob a
/// commit changed, the same path's object in its first parent's tree.
fn delta_bases(store: &dyn Store, ids: &[String], set: &HashSet<&str>) -> Result<HashMap<String, String>> {
    let commits: Vec<&String> = ids.iter().collect();
    // Each commit's pairs on every core, then merged in order: the first
    // commit to change an object picks its base.
    let pairs = parallel(&commits, |id| -> Result<Vec<(String, String)>> {
        let mut hints = Vec::new();
        let Ok(("commit", lo)) = get(store, id) else { return Ok(hints) };
        let hs = headers(split(&lo).unwrap().1);
        let tree = hs.iter().find(|x| x.0 == "tree").map(|x| x.1.to_string());
        let parent = hs.iter().find(|x| x.0 == "parent").map(|x| x.1.to_string());
        let (Some(tree), Some(parent)) = (tree, parent) else { return Ok(hints) };
        let Ok(("commit", plo)) = get(store, &parent) else { return Ok(hints) };
        let Some(old) = headers(split(&plo).unwrap().1).iter().find(|x| x.0 == "tree").map(|x| x.1.to_string()) else {
            return Ok(hints);
        };
        pair_trees(store, &old, &tree, set, &mut hints, &mut HashSet::new())?;
        Ok(hints)
    })?;
    let mut hints = HashMap::new();
    for (n, o) in pairs.into_iter().flatten() {
        hints.entry(n).or_insert(o);
    }
    Ok(hints)
}

fn pair_trees(
    store: &dyn Store,
    old: &str,
    new: &str,
    set: &HashSet<&str>,
    hints: &mut Vec<(String, String)>,
    walked: &mut HashSet<String>,
) -> Result<()> {
    if old == new || !walked.insert(new.to_string()) {
        return Ok(());
    }
    if set.contains(old) && set.contains(new) {
        hints.push((new.to_string(), old.to_string()));
    }
    let (Ok(("tree", olo)), Ok(("tree", nlo))) = (get(store, old), get(store, new)) else { return Ok(()) };
    let olds: HashMap<&[u8], (bool, String)> = tree_entries(split(&olo).unwrap().1)
        .unwrap_or_default()
        .into_iter()
        .map(|(m, n, id)| (n, (m.starts_with('4'), hex(&id))))
        .collect();
    for (m, n, id) in tree_entries(split(&nlo).unwrap().1).unwrap_or_default() {
        let (dir, id) = (m.starts_with('4'), hex(&id));
        let Some((odir, oid)) = olds.get(n) else { continue };
        if *oid == id || dir != *odir || m == "160000" {
            continue;
        }
        if dir {
            pair_trees(store, oid, &id, set, hints, walked)?;
        } else if set.contains(id.as_str()) && set.contains(oid.as_str()) && walked.insert(id.clone()) {
            hints.push((id, oid.clone()));
        }
    }
    Ok(())
}

/// Drop hints so no chain is longer than `max`.
fn limit_depth(hints: &mut HashMap<String, String>, max: usize) {
    let mut depth: HashMap<String, usize> = HashMap::new();
    let keys: Vec<String> = hints.keys().cloned().collect();
    for k in keys {
        // The chain from k down to a known depth or a root.
        let mut chain = vec![k.clone()];
        let mut seen = HashSet::from([k.clone()]);
        let mut d0 = 0;
        loop {
            let last = chain.last().unwrap();
            if let Some(d) = depth.get(last) {
                d0 = *d;
                chain.pop();
                break;
            }
            match hints.get(last) {
                Some(b) if seen.insert(b.clone()) => chain.push(b.clone()),
                Some(_) => {
                    // A cycle: cut it here.
                    let l = last.clone();
                    hints.remove(&l);
                    break;
                }
                None => break,
            }
        }
        let mut d = d0;
        for c in chain.into_iter().rev() {
            if depth.contains_key(&c) {
                continue;
            }
            d = if hints.contains_key(&c) { d + 1 } else { 0 };
            if d > max {
                hints.remove(&c);
                d = 0;
            }
            depth.insert(c, d);
        }
    }
}

/// A git delta turning `base` into `target`, or none when it would not
/// be smaller.
pub fn delta(base: &[u8], target: &[u8]) -> Option<Vec<u8>> {
    const B: usize = 16;
    let hash = |w: &[u8]| -> u64 {
        let x = u128::from_le_bytes(w[..B].try_into().unwrap());
        ((x as u64) ^ ((x >> 64) as u64).rotate_left(29)).wrapping_mul(0x9E37_79B9_7F4A_7C15)
    };
    let mut out = Vec::new();
    let varint = |out: &mut Vec<u8>, mut n: usize| {
        while n >= 0x80 {
            out.push((n as u8) | 0x80);
            n >>= 7;
        }
        out.push(n as u8);
    };
    varint(&mut out, base.len());
    varint(&mut out, target.len());
    let mut index: HashMap<u64, Vec<u32>> = HashMap::new();
    if base.len() >= B {
        let mut i = 0;
        while i + B <= base.len() {
            let v = index.entry(hash(&base[i..])).or_default();
            if v.len() < 8 {
                v.push(i as u32);
            }
            i += B;
        }
    }
    let mut pending: Vec<u8> = Vec::new();
    let flush = |out: &mut Vec<u8>, pending: &mut Vec<u8>| {
        for c in pending.chunks(127) {
            out.push(c.len() as u8);
            out.extend_from_slice(c);
        }
        pending.clear();
    };
    let copy = |out: &mut Vec<u8>, mut off: usize, mut len: usize| {
        while len > 0 {
            let n = len.min(0x10000);
            let mut op = 0x80u8;
            let mut args = Vec::new();
            for k in 0..4 {
                let b = (off >> (8 * k)) as u8;
                if b != 0 {
                    op |= 1 << k;
                    args.push(b);
                }
            }
            if n != 0x10000 {
                for k in 0..3 {
                    let b = (n >> (8 * k)) as u8;
                    if b != 0 {
                        op |= 0x10 << k;
                        args.push(b);
                    }
                }
            }
            out.push(op);
            out.extend_from_slice(&args);
            off += n;
            len -= n;
        }
    };
    let mut i = 0;
    while i < target.len() {
        let mut best: Option<(usize, usize)> = None;
        if i + B <= target.len()
            && let Some(cands) = index.get(&hash(&target[i..]))
        {
            for &c in cands {
                let c = c as usize;
                let n = base[c..].iter().zip(&target[i..]).take_while(|(a, b)| a == b).count();
                if n >= B && best.is_none_or(|(_, bn)| n > bn) {
                    best = Some((c, n));
                }
            }
        }
        match best {
            Some((mut c, mut n)) => {
                let at = i + n;
                // Grow the match back into what was to be inserted.
                while c > 0 && !pending.is_empty() && base[c - 1] == *pending.last().unwrap() {
                    pending.pop();
                    c -= 1;
                    n += 1;
                }
                flush(&mut out, &mut pending);
                copy(&mut out, c, n);
                i = at;
            }
            None => {
                pending.push(target[i]);
                i += 1;
            }
        }
        if out.len() + pending.len() > target.len() {
            return None;
        }
    }
    flush(&mut out, &mut pending);
    Some(out)
}

/// The objects to send someone who wants `wants` and has `haves`: commits
/// the haves do not reach, with whatever of their trees the haves' own
/// trees do not hold, and any tags wanted. Haves this store lacks are
/// ignored.
pub fn closure(store: &dyn Store, wants: &[String], haves: &[String]) -> Result<Vec<String>> {
    let haves: Vec<&String> = haves.iter().filter(|h| has(store, h).unwrap_or(false)).collect();
    // Commits the haves reach, and the objects of their trees.
    let mut seen_commits: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = haves.iter().map(|h| (*h).clone()).collect();
    let mut have_trees = Vec::new();
    while let Some(c) = queue.pop_front() {
        if !seen_commits.insert(c.clone()) || !has(store, &c)? {
            continue;
        }
        let (kind, lo) = get(store, &c)?;
        if kind != "commit" {
            continue;
        }
        for v in children(&lo) {
            match v.known_format().map(|f| f.alias) {
                Some("git-commit") => queue.push_back(id_of(&v).unwrap().to_string()),
                Some("git-tree") if haves.iter().any(|h| **h == c) => have_trees.push(id_of(&v).unwrap().to_string()),
                _ => {}
            }
        }
    }
    let mut known: HashSet<String> = HashSet::new();
    for t in have_trees {
        walk_tree(store, &t, &mut known, &mut Vec::new())?;
    }
    // What the wants add.
    let mut out = Vec::new();
    let mut queue: VecDeque<String> = wants.iter().cloned().collect();
    while let Some(id) = queue.pop_front() {
        if seen_commits.contains(&id) || known.contains(&id) {
            continue;
        }
        let (kind, lo) = get(store, &id)?;
        match kind {
            "commit" => {
                seen_commits.insert(id.clone());
                out.push(id.clone());
                for v in children(&lo) {
                    let cid = id_of(&v).unwrap().to_string();
                    match v.known_format().map(|f| f.alias) {
                        Some("git-commit") => queue.push_back(cid),
                        _ => walk_tree(store, &cid, &mut known, &mut out)?,
                    }
                }
            }
            "tag" => {
                known.insert(id.clone());
                out.push(id.clone());
                queue.extend(children(&lo).iter().map(|v| id_of(v).unwrap().to_string()));
            }
            "tree" => walk_tree(store, &id, &mut known, &mut out)?,
            _ => {
                if known.insert(id.clone()) {
                    out.push(id);
                }
            }
        }
    }
    Ok(out)
}

/// Add tree `id` and everything under it that `known` lacks to `out`.
fn walk_tree(store: &dyn Store, id: &str, known: &mut HashSet<String>, out: &mut Vec<String>) -> Result<()> {
    if !known.insert(id.to_string()) {
        return Ok(());
    }
    out.push(id.to_string());
    let (_, lo) = get(store, id)?;
    for v in children(&lo) {
        let cid = id_of(&v).unwrap();
        if v.known_format().is_some_and(|f| f.alias == "git-tree") {
            walk_tree(store, cid, known, out)?;
        } else if known.insert(cid.to_string()) {
            out.push(cid.to_string());
        }
    }
    Ok(())
}

// ---- Links between nodes and commits ----

/// Which git commit each node of a history is: a `git-links` blob, a
/// `uni-bin` map from node ids to commit ids (40 hex digits).
pub type Links = BTreeMap<NodeId, String>;

pub fn write_links(store: &dyn Store, alg: &'static Algorithm, links: &Links) -> Result<View> {
    let v = Value::map(links.iter().map(|(n, c)| (Value::str(n.as_str()), Value::str(c.as_str()))));
    crate::put(store, alg, "git-links", &v.serialize())
}

pub fn read_links(store: &dyn Store, v: &View) -> Result<Links> {
    let val = Value::deserialize(&read_blob(store, v.key())?).map_err(|e| malformed(v.key(), e))?;
    let mut out = Links::new();
    for (k, c) in val.as_map().ok_or_else(|| malformed(v.key(), "not a map"))? {
        let (Some(k), Some(c)) = (k.as_str(), c.as_str()) else { return Err(malformed(v.key(), "not strings")) };
        out.insert(NodeId::parse(k).map_err(|e| malformed(v.key(), e))?, c.to_string());
    }
    Ok(out)
}

pub(crate) fn check_links(b: &[u8]) -> std::result::Result<(), String> {
    let v = Value::deserialize(b)?;
    for (k, c) in v.as_map().ok_or("not a map")? {
        let ok = k.as_str().is_some_and(|k| NodeId::parse(k).is_ok()) && c.as_str().is_some_and(is_hex40);
        if !ok {
            return Err("a link is a node id and a commit id".into());
        }
    }
    Ok(())
}

pub(crate) fn links_children(b: &[u8]) -> Vec<View> {
    let Ok(v) = Value::deserialize(b) else { return Vec::new() };
    v.as_map()
        .map(|m| m.values().filter_map(|c| c.as_str().filter(|c| is_hex40(c)).map(|c| view("commit", c))).collect())
        .unwrap_or_default()
}

// ---- Annotated tags ----

/// Git's annotated tags: tag name to tag object id, a `git-tags` blob.
pub type Tags = BTreeMap<String, String>;

pub fn write_tags(store: &dyn Store, alg: &'static Algorithm, tags: &Tags) -> Result<View> {
    let v = Value::map(tags.iter().map(|(n, t)| (Value::str(n.as_str()), Value::str(t.as_str()))));
    crate::put(store, alg, "git-tags", &v.serialize())
}

pub fn read_tags(store: &dyn Store, v: &View) -> Result<Tags> {
    let val = Value::deserialize(&read_blob(store, v.key())?).map_err(|e| malformed(v.key(), e))?;
    let mut out = Tags::new();
    for (k, t) in val.as_map().ok_or_else(|| malformed(v.key(), "not a map"))? {
        let (Some(k), Some(t)) = (k.as_str(), t.as_str()) else { return Err(malformed(v.key(), "not strings")) };
        out.insert(k.to_string(), t.to_string());
    }
    Ok(out)
}

pub(crate) fn check_tags(b: &[u8]) -> std::result::Result<(), String> {
    let v = Value::deserialize(b)?;
    for (k, t) in v.as_map().ok_or("not a map")? {
        if !(k.as_str().is_some_and(|k| !k.is_empty()) && t.as_str().is_some_and(is_hex40)) {
            return Err("a tag is a name and a tag object's id".into());
        }
    }
    Ok(())
}

pub(crate) fn tags_children(b: &[u8]) -> Vec<View> {
    let Ok(v) = Value::deserialize(b) else { return Vec::new() };
    v.as_map()
        .map(|m| m.values().filter_map(|t| t.as_str().filter(|t| is_hex40(t)).map(|t| view("tag", t))).collect())
        .unwrap_or_default()
}

/// The object an annotated tag names, if `id` is a tag.
pub fn peel_tag(store: &dyn Store, id: &str) -> Option<String> {
    let (kind, lo) = get(store, id).ok()?;
    if kind != "tag" {
        return None;
    }
    let body = split(&lo)?.1;
    headers(body).into_iter().find(|(k, _)| *k == "object").map(|(_, v)| v.to_string())
}

// ---- cas to git ----

/// A file's git mode and contents.
fn file_of(store: &dyn Store, v: &View) -> Result<(&'static str, Vec<u8>)> {
    let bytes = read_blob(store, v.key())?;
    Ok(match v.known_format().map(|f| f.alias) {
        Some("exec") => ("100755", bytes),
        Some("symlink") => ("120000", bytes),
        Some("git-blob") => ("100644", crate::formats::git_blob_contents(&bytes).map(<[u8]>::to_vec).unwrap_or(bytes)),
        _ => ("100644", bytes),
    })
}

/// Converts versions to git trees, each directory once.
pub struct TreeWriter<'a> {
    store: &'a dyn Store,
    trees: HashMap<String, String>,
    batch: Batch<'a>,
}

impl<'a> TreeWriter<'a> {
    pub fn new(store: &'a dyn Store) -> TreeWriter<'a> {
        TreeWriter { store, trees: HashMap::new(), batch: Batch::new(store) }
    }

    /// Write what is queued.
    pub fn flush(&mut self) -> Result<()> {
        self.batch.flush()
    }

    /// The git tree of directory `dir`.
    pub fn tree(&mut self, dir: &View) -> Result<String> {
        if let Some(t) = self.trees.get(dir.key()) {
            return Ok(t.clone());
        }
        let mut entries: Vec<(Vec<u8>, String, [u8; 20])> = Vec::new();
        for it in crate::tree::load(self.store, dir)? {
            let (mode, id) = if it.child.is_dir() {
                ("40000", self.tree(&it.child)?)
            } else {
                let (mode, bytes) = file_of(self.store, &it.child)?;
                // Nearly the file's own blob: a tiny delta on it.
                (mode, self.batch.put("blob", &bytes, Some(it.child.key().to_string()))?)
            };
            let mut raw = [0u8; 20];
            for (k, b) in raw.iter_mut().enumerate() {
                *b = u8::from_str_radix(&id[2 * k..2 * k + 2], 16).unwrap();
            }
            entries.push((it.name.into_bytes(), mode.to_string(), raw));
        }
        // Git's order: a tree's name sorts as if it ended in `/`.
        entries.sort_by(|a, b| {
            let key = |e: &(Vec<u8>, String, [u8; 20])| {
                let mut k = e.0.clone();
                if e.1 == "40000" {
                    k.push(b'/');
                }
                k
            };
            key(a).cmp(&key(b))
        });
        let mut body = Vec::new();
        for (name, mode, id) in &entries {
            body.extend_from_slice(mode.as_bytes());
            body.push(b' ');
            body.extend_from_slice(name);
            body.push(0);
            body.extend_from_slice(id);
        }
        let t = self.batch.put("tree", &body, None)?;
        self.trees.insert(dir.key().to_string(), t.clone());
        Ok(t)
    }
}

/// Give every node of `h` that `links` lacks a git commit: its version's
/// tree, its predecessors' commits as parents, and what its step says as
/// the message, by `cas <cas@localhost>` at time 0, so the same history
/// always gives the same commits. Nodes already linked keep their commits
/// (a history that came from git keeps git's ids).
pub fn to_git(store: &dyn Store, h: &History, links: &mut Links) -> Result<usize> {
    to_git_from(store, h, links, &[])
}

/// [`to_git`], where a node of `h` that one of `others` holds with the
/// same predecessors, themselves the same commits, takes that history's
/// commit: a node shared by two histories is one commit.
pub fn to_git_from(store: &dyn Store, h: &History, links: &mut Links, others: &[Other<'_>]) -> Result<usize> {
    let mut tw = TreeWriter::new(store);
    let mut made = 0;
    for (id, n) in predecessors_first(h.nodes.clone()) {
        if links.contains_key(&id) {
            continue;
        }
        let shared = others.iter().find_map(|(oh, ol)| {
            let c = ol.get(&id)?;
            let on = oh.nodes.get(&id)?;
            let same = on.after.len() == n.after.len()
                && n.after
                    .keys()
                    .all(|p| on.after.contains_key(p) && links.get(p).is_some() && links.get(p) == ol.get(p));
            same.then(|| c.clone())
        });
        if let Some(c) = shared {
            links.insert(id, c);
            continue;
        }
        let tree = tw.tree(&id.version())?;
        let mut body = format!("tree {tree}\n");
        for p in n.after.keys() {
            let pc = links.get(p).ok_or_else(|| Error::Missing(format!("commit of node {p}")))?;
            body.push_str(&format!("parent {pc}\n"));
        }
        let msg = match n.after.values().next() {
            Some(s) => s.line(store)?.or_else(|| s.changelog.clone()),
            None => None,
        }
        .or_else(|| n.doc.clone())
        .unwrap_or_else(|| "(no notes)".to_string());
        body.push_str("author cas <cas@localhost> 0 +0000\ncommitter cas <cas@localhost> 0 +0000\n\n");
        body.push_str(msg.trim_end());
        body.push('\n');
        links.insert(id, tw.batch.put("commit", body.as_bytes(), None)?);
        made += 1;
    }
    tw.flush()?;
    Ok(made)
}

// ---- git to cas ----

/// Convert the commits reachable from `tip` (a commit id) into a history,
/// through the git importer (spec/git-import.md) run on a scratch
/// repository made from the stored objects; the links say which commit
/// each node is. Needs the `git` command.
pub fn from_git(
    store: &dyn Store,
    alg: &'static Algorithm,
    tip: &str,
    tags: &[(String, String)],
) -> Result<(History, Links)> {
    let dir = scratch()?;
    let r = (|| {
        run(&dir, &["init", "-q", "--bare", "."], None)?;
        let mut wants = vec![tip.to_string()];
        wants.extend(tags.iter().map(|t| t.1.clone()));
        let ids = closure(store, &wants, &[])?;
        run(&dir, &["index-pack", "--stdin"], Some(&pack(store, &ids)?))?;
        run(&dir, &["update-ref", "refs/heads/cas", tip], None)?;
        for (name, id) in tags {
            run(&dir, &["update-ref", &format!("refs/tags/{name}"), id], None)?;
        }
        let (h, im) = crate::git::import(store, alg, &dir, "refs/heads/cas")?;
        let links = im.nodes.into_iter().map(|(c, n)| (n, c)).collect();
        Ok((h, links))
    })();
    let _ = std::fs::remove_dir_all(&dir);
    r
}

fn scratch() -> Result<std::path::PathBuf> {
    let base = std::env::temp_dir();
    for n in 0..1000u32 {
        let d = base.join(format!("cas-git-{}-{n}", std::process::id()));
        if std::fs::create_dir(&d).is_ok() {
            return Ok(d);
        }
    }
    Err(Error::Io("cannot make a scratch directory".into()))
}

fn run(dir: &std::path::Path, args: &[&str], stdin: Option<&[u8]>) -> Result<Vec<u8>> {
    let mut c = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(if stdin.is_some() { std::process::Stdio::piped() } else { std::process::Stdio::null() })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    if let Some(b) = stdin {
        let mut w = c.stdin.take().unwrap();
        let b = b.to_vec();
        let t = std::thread::spawn(move || w.write_all(&b));
        let out = c.wait_with_output()?;
        let _ = t.join();
        if !out.status.success() {
            return Err(Error::Io(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim())));
        }
        return Ok(out.stdout);
    }
    let out = c.wait_with_output()?;
    if !out.status.success() {
        return Err(Error::Io(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim())));
    }
    Ok(out.stdout)
}

/// Read a whole stream (a request body).
pub fn read_all(mut r: impl Read) -> Result<Vec<u8>> {
    let mut v = Vec::new();
    r.read_to_end(&mut v)?;
    Ok(v)
}

/// Whether `v` names a git object (one of the four formats).
pub fn is_git(v: &View) -> bool {
    v.known_format().is_some_and(|f| kind_of(f.alias).is_some())
}

// ---- git to cas, in Rust: what a push brings ----

/// Converts git trees to versions (the `tree` directory format, as the
/// importer writes them), each file once.
pub struct VersionWriter<'a> {
    store: &'a dyn Store,
    alg: &'static Algorithm,
    batch: Batch<'a>,
    files: HashMap<(String, &'static str), View>,
    versions: HashMap<String, View>,
}

impl<'a> VersionWriter<'a> {
    pub fn new(store: &'a dyn Store, alg: &'static Algorithm) -> VersionWriter<'a> {
        VersionWriter { store, alg, batch: Batch::new(store), files: HashMap::new(), versions: HashMap::new() }
    }

    fn collect(
        &mut self,
        tree: &str,
        at: Option<&crate::Path>,
        out: &mut BTreeMap<crate::Path, crate::Node>,
    ) -> Result<()> {
        let (kind, lo) = get(self.store, tree)?;
        if kind != "tree" {
            return Err(malformed(&format!("sha1:{tree}"), "not a tree"));
        }
        let entries = tree_entries(split(&lo).unwrap().1).map_err(|e| malformed(&format!("sha1:{tree}"), e))?;
        if entries.is_empty()
            && let Some(p) = at
        {
            out.insert(p.clone(), crate::Node::Dir);
        }
        for (mode, name, id) in entries {
            let name = std::str::from_utf8(name).map_err(|_| Error::BadPath(String::from_utf8_lossy(name).into()))?;
            let path = match at {
                Some(p) => crate::Path::new(format!("{p}/{name}"))?,
                None => crate::Path::new(name)?,
            };
            let id = hex(&id);
            match mode {
                "40000" | "040000" => self.collect(&id, Some(&path), out)?,
                "160000" => {}
                m => {
                    let alias = match m {
                        "100755" => "exec",
                        "120000" => "symlink",
                        _ => "raw",
                    };
                    let v = match self.files.get(&(id.clone(), alias)) {
                        Some(v) => v.clone(),
                        None => {
                            let (_, lo) = get(self.store, &id)?;
                            let body = split(&lo).unwrap().1;
                            let f = by_alias(alias).unwrap();
                            let key = self.alg.key(body);
                            f.check(&key, body)?;
                            // A tiny delta on git's blob of the same file.
                            self.batch.value(key.clone(), body.to_vec(), Some(format!("sha1:{id}")))?;
                            let v = View { format: f.id().to_string(), key };
                            self.files.insert((id, alias), v.clone());
                            v
                        }
                    };
                    out.insert(path, crate::Node::File(v));
                }
            }
        }
        Ok(())
    }

    /// Write what is queued.
    pub fn flush(&mut self) -> Result<()> {
        self.batch.flush()
    }

    /// The version of git tree `tree`.
    pub fn version(&mut self, tree: &str) -> Result<View> {
        if let Some(v) = self.versions.get(tree) {
            return Ok(v.clone());
        }
        let mut entries = BTreeMap::new();
        self.collect(tree, None, &mut entries)?;
        let v = crate::build(self.store, self.alg, by_alias("tree").unwrap(), &entries)?;
        self.versions.insert(tree.to_string(), v.clone());
        Ok(v)
    }
}

/// Add to `h` the commits `tip` reaches that `links` does not name yet,
/// parents first, and link them; the node of `tip`. A commit becomes a
/// node whose version is its tree; the step from its first parent has its
/// message as the step's note (a root commit's message is its node's doc).
pub fn extend(
    store: &dyn Store,
    alg: &'static Algorithm,
    h: &mut History,
    links: &mut Links,
    tip: &str,
) -> Result<NodeId> {
    extend_from(store, alg, h, links, tip, &[])
}

/// Copy node `id` and every node before it from `from` into `h`, with
/// their commits from `from_links` into `links`, where `h` lacks them. A
/// node `h` holds already must be the same commit.
fn graft(h: &mut History, links: &mut Links, from: &History, from_links: &Links, id: &NodeId) -> Result<()> {
    let mut stack = vec![id.clone()];
    while let Some(n) = stack.pop() {
        let c = from_links.get(&n).ok_or_else(|| Error::Missing(format!("commit of node {n}")))?;
        if h.nodes.contains_key(&n) {
            if links.get(&n) != Some(c) {
                return Err(Error::Diverged(format!("node {n} is another commit here")));
            }
            continue;
        }
        let Some(node) = from.nodes.get(&n) else { return Err(Error::Missing(format!("node {n}"))) };
        stack.extend(node.after.keys().cloned());
        h.nodes.insert(n.clone(), node.clone());
        links.insert(n, c.clone());
    }
    Ok(())
}

/// Another history and its links, whose commits a conversion may share.
pub type Other<'a> = (&'a History, &'a Links);

/// [`extend`], where commits may already be nodes of `others`: such a
/// commit brings its node, and what precedes it, into `h`, so histories
/// that share commits share nodes.
pub fn extend_from(
    store: &dyn Store,
    alg: &'static Algorithm,
    h: &mut History,
    links: &mut Links,
    tip: &str,
    others: &[Other<'_>],
) -> Result<NodeId> {
    // Each known commit: its node, and the history it is in (`None`: `h`).
    let mut node_of: HashMap<String, (NodeId, Option<usize>)> =
        links.iter().map(|(n, c)| (c.clone(), (n.clone(), None))).collect();
    for (i, (_, l)) in others.iter().enumerate() {
        for (n, c) in l.iter() {
            node_of.entry(c.clone()).or_insert((n.clone(), Some(i)));
        }
    }
    let bring = |h: &mut History, links: &mut Links, (n, from): &(NodeId, Option<usize>)| -> Result<NodeId> {
        if let Some(i) = from {
            graft(h, links, others[*i].0, others[*i].1, n)?;
        }
        Ok(n.clone())
    };
    if let Some(k) = node_of.get(tip) {
        return bring(h, links, k);
    }
    // The new commits, parents before children.
    let mut order: Vec<(String, String, Vec<String>, String)> = Vec::new();
    let mut state: HashMap<String, bool> = HashMap::new();
    let mut stack = vec![(tip.to_string(), false)];
    let mut info: HashMap<String, (String, Vec<String>, String)> = HashMap::new();
    while let Some((c, done)) = stack.pop() {
        if done {
            let (t, ps, m) = info.remove(&c).unwrap();
            order.push((c, t, ps, m));
            continue;
        }
        if node_of.contains_key(&c) || state.contains_key(&c) {
            continue;
        }
        state.insert(c.clone(), true);
        let (kind, lo) = get(store, &c)?;
        if kind != "commit" {
            return Err(malformed(&format!("sha1:{c}"), "not a commit"));
        }
        let body = split(&lo).unwrap().1;
        let hs = headers(body);
        let tree = hs.iter().find(|x| x.0 == "tree").map(|x| x.1.to_string()).unwrap_or_default();
        let parents: Vec<String> = hs.iter().filter(|x| x.0 == "parent").map(|x| x.1.to_string()).collect();
        let msg = body.windows(2).position(|w| w == b"\n\n").map(|i| &body[i + 2..]).unwrap_or(b"");
        let msg = String::from_utf8_lossy(msg).trim_end().to_string();
        stack.push((c.clone(), true));
        for p in parents.iter().rev() {
            stack.push((p.clone(), false));
        }
        info.insert(c, (tree, parents, msg));
    }
    let mut vw = VersionWriter::new(store, alg);
    let mut last = None;
    for (c, tree, parents, msg) in order {
        let version = vw.version(&tree)?;
        let mut after = BTreeMap::new();
        for (i, p) in parents.iter().enumerate() {
            let k = node_of.get(p).ok_or_else(|| Error::Missing(format!("node of commit {p}")))?.clone();
            let pn = bring(h, links, &k)?;
            let mut step = crate::history::Step::default();
            if i == 0 && !msg.is_empty() {
                let sn = crate::stepnotes::StepNotes {
                    before: Some(pn.version()),
                    after: version.clone(),
                    note: msg.lines().filter(|l| !l.trim().is_empty()).collect::<Vec<_>>().join(" / "),
                    paths: BTreeMap::new(),
                };
                step.notes = Some(crate::stepnotes::write(store, alg, &sn)?);
            }
            after.insert(pn, step);
        }
        let doc = (parents.is_empty() && !msg.is_empty()).then(|| msg.clone());
        let id = h.add(&version, after, doc)?;
        links.insert(id.clone(), c.clone());
        node_of.insert(c, (id.clone(), None));
        last = Some(id);
    }
    vw.flush()?;
    last.ok_or_else(|| Error::Missing(format!("commit {tip}")))
}

#[cfg(test)]
mod delta_tests {
    #[test]
    fn deltas_apply() {
        let base: Vec<u8> = (0..5000u32).flat_map(|i| format!("line {i}\n").into_bytes()).collect();
        let mut target = base.clone();
        target.splice(1000..1010, b"changed here".iter().copied());
        target.extend_from_slice(b"the end\n");
        let target: Vec<u8> = b"new start\n".iter().copied().chain(target).collect();
        let d = super::delta(&base, &target).unwrap();
        assert!(d.len() < 200, "{}", d.len());
        assert_eq!(super::apply_delta(&base, &d).unwrap(), target);
        let big = vec![7u8; 300_000];
        let d = super::delta(&big, &big).unwrap();
        assert_eq!(super::apply_delta(&big, &d).unwrap(), big);
        assert!(super::delta(b"abc", b"something else entirely").is_none());
    }
}
