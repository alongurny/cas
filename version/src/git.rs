//! Import a git repository's history (spec/git-import.md): git trees become
//! `dir` objects, files become `raw`, `exec` or `symlink` objects, commits
//! become history nodes, commit messages become step changelogs, and git
//! tags become history tags. Runs the `git` command; reads only.

use crate::dir::{Item, TREE};
use crate::history::{History, NodeId, Step};
use crate::{Error, Path, Result, View, by_alias};
use cas_core::Store;
use cas_core::algo::Algorithm;
use cas_core::store::Put;
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path as FsPath;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

fn git(repo: &FsPath, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("git").arg("-C").arg(repo).args(args).output()?;
    if !out.status.success() {
        return Err(Error::Io(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim())));
    }
    Ok(out.stdout)
}

/// A git object id.
type Oid = [u8; 20];

/// Hashes a key by the first 8 bytes of its git id, which are already
/// uniformly random: SipHash over every lookup cost a tenth of an import.
#[derive(Default)]
struct OidHasher {
    h: u64,
    seeded: bool,
}

impl std::hash::Hasher for OidHasher {
    fn finish(&self) -> u64 {
        self.h
    }
    fn write(&mut self, b: &[u8]) {
        if !self.seeded && b.len() >= 8 {
            self.h ^= u64::from_le_bytes(b[..8].try_into().unwrap());
            self.seeded = true;
        } else {
            for &x in b {
                self.h = (self.h.rotate_left(5) ^ u64::from(x)).wrapping_mul(0x100000001b3);
            }
        }
    }
    // Length prefixes and the like: mixed in cheaply.
    fn write_usize(&mut self, n: usize) {
        self.h = (self.h ^ n as u64).wrapping_mul(0x9e3779b97f4a7c15);
    }
}

type OidMap<K, V> = HashMap<K, V, std::hash::BuildHasherDefault<OidHasher>>;
type OidSet<K> = std::collections::HashSet<K, std::hash::BuildHasherDefault<OidHasher>>;
/// A directory's leaves (greatest name, key) and the blobs above them.
type DirBlobs = (Vec<(String, String)>, Vec<String>);

/// Fetched file contents by git id and format, not written yet.
type Bodies = OidMap<(Oid, &'static str), Vec<u8>>;

fn oid(hex: &str) -> Result<Oid> {
    let b = hex.as_bytes();
    let nib = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    if b.len() != 40 {
        return Err(Error::Io(format!("bad git id {hex:?}")));
    }
    let mut o = [0; 20];
    for (i, x) in o.iter_mut().enumerate() {
        *x = nib(b[2 * i])
            .zip(nib(b[2 * i + 1]))
            .map(|(h, l)| h << 4 | l)
            .ok_or_else(|| Error::Io(format!("bad git id {hex:?}")))?;
    }
    Ok(o)
}

fn hex(o: &Oid) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    o.iter().flat_map(|b| [HEX[usize::from(b >> 4)] as char, HEX[usize::from(b & 15)] as char]).collect()
}

/// A running `git cat-file --batch-command --buffer`: requests go in
/// batches, answered in order, so a batch costs one round trip.
struct CatFile {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
}

impl CatFile {
    fn start(repo: &FsPath) -> Result<CatFile> {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["cat-file", "--batch-command", "--buffer"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let input = Some(child.stdin.take().unwrap());
        let output = BufReader::with_capacity(1 << 20, child.stdout.take().unwrap());
        Ok(CatFile { child, input, output })
    }

    /// The contents of each of `ids`, in order.
    fn get_many(&mut self, ids: &[Oid]) -> Result<Vec<Vec<u8>>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut req = String::with_capacity(ids.len() * 50 + 6);
        for id in ids {
            req.push_str("contents ");
            req.push_str(&hex(id));
            req.push('\n');
        }
        req.push_str("flush\n");
        let input = self.input.as_mut().unwrap();
        // A writer thread, so that git's answers never wait on our requests.
        std::thread::scope(|sc| {
            let w = sc.spawn(move || input.write_all(req.as_bytes()).and_then(|_| input.flush()));
            let mut out = Vec::with_capacity(ids.len());
            let mut header = String::new();
            for _ in ids {
                header.clear();
                self.output.read_line(&mut header)?;
                let size: usize = header
                    .split_whitespace()
                    .nth(2)
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| Error::Io(format!("git cat-file: {}", header.trim())))?;
                let mut body = vec![0; size + 1];
                self.output.read_exact(&mut body)?;
                body.pop();
                out.push(body);
            }
            w.join().map_err(|_| Error::Io("git cat-file writer panicked".into()))??;
            Ok(out)
        })
    }
}

impl Drop for CatFile {
    fn drop(&mut self) {
        drop(self.input.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// What an import did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Imported {
    pub commits: usize,
    pub trees: usize,
    pub files: usize,
    /// Submodule entries, which have no content here and are left out.
    pub skipped: usize,
}

/// One entry of a git tree.
enum Entry<'a> {
    Tree(&'a str, Oid),
    File(&'a str, Oid, &'static str),
    Skipped,
}

impl<'a> Entry<'a> {
    /// Name, whether a tree, and id.
    fn parts(self) -> Option<(&'a str, bool, Oid)> {
        match self {
            Entry::Tree(n, c) => Some((n, true, c)),
            Entry::File(n, c, _) => Some((n, false, c)),
            Entry::Skipped => None,
        }
    }
}

/// A tree's entries as (name, whether a tree, id).
type Listed<'a> = Vec<(&'a str, bool, Oid)>;

/// Git's tree order: by name, a tree's name as if it ended in '/'.
fn git_order(a: &str, a_tree: bool, b: &str, b_tree: bool) -> std::cmp::Ordering {
    let a = a.bytes().chain(a_tree.then_some(b'/'));
    a.cmp(b.bytes().chain(b_tree.then_some(b'/')))
}

/// [`git_order`] on name bytes.
fn git_order_bytes(a: &[u8], a_tree: bool, b: &[u8], b_tree: bool) -> std::cmp::Ordering {
    let a = a.iter().copied().chain(a_tree.then_some(b'/'));
    a.cmp(b.iter().copied().chain(b_tree.then_some(b'/')))
}

/// One raw git tree entry: its bytes, mode, name and id.
#[derive(Clone, Copy)]
struct RawEntry<'a> {
    whole: &'a [u8],
    mode: &'a [u8],
    name: &'a [u8],
    oid: Oid,
}

impl RawEntry<'_> {
    fn tree(&self) -> bool {
        self.mode == b"40000"
    }

    /// The format a file entry imports as (`None` for a tree or a skipped entry).
    fn alias(&self) -> Option<&'static str> {
        match self.mode {
            b"100644" | b"100664" => Some("raw"),
            b"100755" => Some("exec"),
            b"120000" => Some("symlink"),
            _ => None,
        }
    }

    fn skipped(&self) -> bool {
        !self.tree() && self.alias().is_none()
    }
}

/// A git tree's entries as bytes, without checking names: for diffs.
struct RawEntries<'a>(&'a [u8]);

impl<'a> Iterator for RawEntries<'a> {
    type Item = Result<RawEntry<'a>>;

    fn next(&mut self) -> Option<Result<RawEntry<'a>>> {
        let b = self.0;
        if b.is_empty() {
            return None;
        }
        let bad = || Some(Err(Error::Io("bad git tree".into())));
        let Some(sp) = b.iter().take(8).position(|&c| c == b' ') else { return bad() };
        let Some(nul) = b[sp..].iter().position(|&c| c == 0).map(|n| sp + n) else { return bad() };
        let Some(id) = b.get(nul + 1..nul + 21) else { return bad() };
        self.0 = &b[nul + 21..];
        Some(Ok(RawEntry { whole: &b[..nul + 21], mode: &b[..sp], name: &b[sp + 1..nul], oid: id.try_into().unwrap() }))
    }
}

/// Binary tree entries: "<mode> <name>\0<20-byte id>".
fn entries<'a>(id: &Oid, raw: &'a [u8]) -> Result<Vec<Entry<'a>>> {
    let bad = || Error::Io(format!("bad git tree {}", hex(id)));
    let mut out = Vec::new();
    let mut rest = raw;
    while !rest.is_empty() {
        let nul = rest.iter().position(|&b| b == 0).ok_or_else(bad)?;
        let head =
            std::str::from_utf8(&rest[..nul]).map_err(|_| Error::Io(format!("non-UTF-8 name in {}", hex(id))))?;
        let (mode, name) = head.split_once(' ').ok_or_else(bad)?;
        let child: Oid = rest.get(nul + 1..nul + 21).ok_or_else(bad)?.try_into().unwrap();
        rest = &rest[nul + 21..];
        out.push(match mode {
            "40000" => Entry::Tree(name, child),
            "100644" | "100664" => Entry::File(name, child, "raw"),
            "100755" => Entry::File(name, child, "exec"),
            "120000" => Entry::File(name, child, "symlink"),
            _ => Entry::Skipped,
        });
    }
    Ok(out)
}

struct Importer<'a> {
    store: &'a dyn Store,
    alg: &'static Algorithm,
    cat: CatFile,
    /// Blobs not written yet, children before parents; written in batches.
    pending: Vec<Put>,
    pending_bytes: usize,
    trees: OidMap<Oid, View>,
    /// `tree` blobs queued already: big directories share most of theirs.
    chunks: std::collections::HashSet<String>,
    /// Every key queued for writing.
    written: cas_core::keyhash::KeySet,
    files: OidMap<(Oid, &'static str), View>,
    /// Leaf keys by a hash of the git entries they hold.
    leaves: OidMap<[u8; 32], String>,
    /// The key of the last file written at each path: the next version's
    /// likely delta base.
    last_at: HashMap<String, String>,
    /// The leaves (greatest name, key) and the blobs above them of the last
    /// directory converted at each path, for the next one's delta bases.
    last_dir: HashMap<String, DirBlobs>,
    /// A new tree's and a new file's version at the same path in a parent
    /// commit (of a merge, the parent whose tree there shares the most
    /// entries), found while fetching: its delta base. The last version
    /// imported at the path is often on another branch (topic branches
    /// come after newer commits of the branch they fork from); it is only
    /// the fallback.
    tree_pair: OidMap<Oid, Oid>,
    file_pair: OidMap<Oid, Oid>,
    /// [`DirBlobs`] of each converted git tree, for the trees paired with it.
    dir_blobs: OidMap<Oid, DirBlobs>,
    /// Tree bodies of the previous chunk of commits, and of trees fetched
    /// only as counterparts: most first parents' trees are among them.
    old_raw: OidMap<Oid, Vec<u8>>,
    /// Tree bodies fetched only to list a commit's changed paths.
    extra: OidMap<Oid, Vec<u8>>,
    n: Imported,
}

impl Importer<'_> {
    /// [`crate::put`], batched: the write happens at the next flush.
    /// `checked`: the bytes were made here and are valid by construction.
    fn put(&mut self, alias: &str, bytes: Vec<u8>, checked: bool, like: Option<String>) -> Result<View> {
        let f = by_alias(alias).ok_or_else(|| Error::UnknownFormat(alias.to_string()))?;
        let key = self.alg.key(&bytes);
        if checked {
            debug_assert!(f.check(&key, &bytes).is_ok());
        } else {
            f.check(&key, &bytes)?;
        }
        let view = View { format: f.id().to_string(), key: key.clone() };
        // A value met before (the same content under another mode) is not
        // written again: a second write could make it a delta on a blob
        // that is itself a delta on it.
        if !self.written.insert(key.clone()) {
            return Ok(view);
        }
        self.pending_bytes += bytes.len();
        let like = like.filter(|l| *l != key);
        self.pending.push(Put { like, ..Put::blob(key, bytes) });
        if self.pending_bytes >= 128 << 20 {
            self.flush()?;
        }
        Ok(view)
    }

    fn flush(&mut self) -> Result<()> {
        let batch = std::mem::take(&mut self.pending);
        self.pending_bytes = 0;
        self.store.write_many(&batch).map_err(|(_, e)| Error::from(e))
    }

    /// Fetch every tree and file under `roots` that is not converted yet,
    /// one level at a time (one round trip to git each). The trees'
    /// contents are left in `raw`, the files' in `bodies`.
    /// `roots` pairs each root with its parents' roots.
    fn fetch(&mut self, roots: &[(Oid, Vec<Oid>)], raw: &mut OidMap<Oid, Vec<u8>>, bodies: &mut Bodies) -> Result<()> {
        let mut want: Vec<(Oid, Vec<Oid>)> = Vec::new();
        let mut queued = OidSet::default();
        for (r, qs) in roots {
            if !self.trees.contains_key(r) && !raw.contains_key(r) && queued.insert(*r) {
                want.push((*r, qs.iter().copied().filter(|q| q != r).collect()));
            }
        }
        let mut files: Vec<(Oid, &'static str)> = Vec::new();
        while !want.is_empty() || !files.is_empty() {
            // Counterparts whose bodies are not at hand are fetched with
            // the level.
            let mut asked = OidSet::default();
            let at: OidMap<Oid, usize> = want.iter().enumerate().map(|(i, (w, _))| (*w, i)).collect();
            let missing: Vec<Oid> = want
                .iter()
                .flat_map(|(_, qs)| qs.iter().copied())
                .filter(|q| {
                    !raw.contains_key(q) && !self.old_raw.contains_key(q) && !at.contains_key(q) && asked.insert(*q)
                })
                .collect();
            let ids: Vec<Oid> = want
                .iter()
                .map(|(w, _)| *w)
                .chain(missing.iter().copied())
                .chain(files.iter().map(|(o, _)| *o))
                .collect();
            let mut got = self.cat.get_many(&ids)?.into_iter();
            let new_bodies: Vec<Vec<u8>> = want.iter().map(|_| got.next().unwrap()).collect();
            for q in &missing {
                self.old_raw.insert(*q, got.next().unwrap());
            }
            let mut next = Vec::new();
            let mut queued = OidSet::default();
            let mut new_files = Vec::new();
            let mut seen = OidSet::default();
            for ((id, qs), body) in want.iter().zip(&new_bodies) {
                let mine = entries(id, body)?;
                // Each counterpart's entries by name, the one sharing the
                // most entries first: the tree's delta base (a merge's
                // tree is often closer to the branch merged in).
                let mut olds: Vec<(Oid, Listed)> = Vec::new();
                for q in qs {
                    let qb = raw.get(q).or_else(|| self.old_raw.get(q)).or_else(|| at.get(q).map(|i| &new_bodies[*i]));
                    if let Some(qb) = qb {
                        olds.push((*q, entries(q, qb)?.into_iter().filter_map(Entry::parts).collect()));
                    }
                }
                let mine: Vec<(&str, bool, Oid, Option<&'static str>)> = mine
                    .into_iter()
                    .filter_map(|e| match e {
                        Entry::Tree(n, c) => Some((n, true, c, None)),
                        Entry::File(n, c, a) => Some((n, false, c, Some(a))),
                        Entry::Skipped => None,
                    })
                    .collect();
                // Both lists are in git's order: walk them together.
                let matches = |old: &[(&str, bool, Oid)]| -> Vec<Option<Oid>> {
                    let mut j = 0;
                    mine.iter()
                        .map(|&(n, t, _, _)| {
                            while j < old.len() && git_order(old[j].0, old[j].1, n, t).is_lt() {
                                j += 1;
                            }
                            (j < old.len() && old[j].0 == n && old[j].1 == t).then(|| old[j].2)
                        })
                        .collect()
                };
                let mut found: Vec<(Oid, Vec<Option<Oid>>)> = olds.iter().map(|(q, o)| (*q, matches(o))).collect();
                if found.len() > 1 {
                    let score = |m: &[Option<Oid>]| mine.iter().zip(m).filter(|(e, o)| **o == Some(e.2)).count();
                    let best = (0..found.len()).max_by_key(|&i| (score(&found[i].1), std::cmp::Reverse(i))).unwrap();
                    found.swap(0, best);
                }
                if let Some((q, _)) = found.first() {
                    self.tree_pair.entry(*id).or_insert(*q);
                }
                for (i, &(_, tree, c, alias)) in mine.iter().enumerate() {
                    let olds_here = found.iter().filter_map(|(_, m)| m[i]).filter(|o| *o != c);
                    match alias {
                        None => {
                            if !self.trees.contains_key(&c) && !raw.contains_key(&c) && queued.insert(c) {
                                let mut qs: Vec<Oid> = Vec::new();
                                for o in olds_here {
                                    if !qs.contains(&o) {
                                        qs.push(o);
                                    }
                                }
                                next.push((c, qs))
                            }
                        }
                        Some(alias) => {
                            debug_assert!(!tree);
                            if let Some(qc) = olds_here.into_iter().next() {
                                self.file_pair.entry(c).or_insert(qc);
                            }
                            if !self.files.contains_key(&(c, alias)) && seen.insert((c, alias)) {
                                new_files.push((c, alias))
                            }
                        }
                    }
                }
            }
            for ((id, _), body) in want.iter().zip(new_bodies) {
                raw.insert(*id, body);
            }
            for f in std::mem::take(&mut files) {
                bodies.insert(f, got.next().unwrap());
            }
            files = new_files;
            want = next;
        }
        Ok(())
    }

    /// The paths whose entries differ between git trees `t` and `q` (a
    /// commit's and its first parent's), with every path under a tree
    /// only one side has, as [`crate::diff`] lists them: a step's filter
    /// for the history graph. `None` past [`crate::graph::MAX_PATHS`].
    /// Bodies come from this chunk's and the last one's trees; the few
    /// others (a tree met before on another branch) are fetched.
    fn changed(&mut self, raw: &OidMap<Oid, Vec<u8>>, t: Oid, q: Oid) -> Result<Option<Vec<String>>> {
        let mut out: Vec<String> = Vec::new();
        let mut todo: Vec<(Option<Oid>, Option<Oid>, String)> = vec![(Some(t), Some(q), String::new())];
        while let Some((a, b, prefix)) = todo.pop() {
            let missing: Vec<Oid> = [a, b]
                .into_iter()
                .flatten()
                .filter(|o| !raw.contains_key(o) && !self.old_raw.contains_key(o) && !self.extra.contains_key(o))
                .collect();
            if !missing.is_empty() {
                for (o, body) in missing.iter().zip(self.cat.get_many(&missing)?) {
                    self.extra.insert(*o, body);
                }
            }
            let body = |o: &Option<Oid>| match o {
                Some(o) => raw.get(o).or_else(|| self.old_raw.get(o)).or_else(|| self.extra.get(o)).unwrap().as_slice(),
                None => &[],
            };
            let (mut ia, mut ib) = (RawEntries(body(&a)), RawEntries(body(&b)));
            let (mut x, mut y) = (ia.next().transpose()?, ib.next().transpose()?);
            loop {
                let (here, there) = match (x, y) {
                    (None, None) => break,
                    // The same entry, byte for byte: most of a tree.
                    (Some(p), Some(q)) if p.whole == q.whole => {
                        (x, y) = (ia.next().transpose()?, ib.next().transpose()?);
                        continue;
                    }
                    (Some(p), Some(q)) => match git_order_bytes(p.name, p.tree(), q.name, q.tree()) {
                        std::cmp::Ordering::Less => (x, None),
                        std::cmp::Ordering::Greater => (None, y),
                        std::cmp::Ordering::Equal => (x, y),
                    },
                    (Some(_), None) => (x, None),
                    (None, Some(_)) => (None, y),
                };
                if here.is_some() {
                    x = ia.next().transpose()?;
                }
                if there.is_some() {
                    y = ib.next().transpose()?;
                }
                let e = here.or(there).unwrap();
                // Submodules have no CAS form; a mode git keeps but this
                // import does not tell apart is still a change of bytes.
                if here.is_none_or(|h| h.skipped()) && there.is_none_or(|t| t.skipped()) {
                    continue;
                }
                if let (Some(h), Some(t)) = (here, there)
                    && h.oid == t.oid
                    && h.alias() == t.alias()
                {
                    continue;
                }
                let name = std::str::from_utf8(e.name).map_err(|_| Error::Io("non-UTF-8 name in a git tree".into()))?;
                let path = if prefix.is_empty() { name.to_string() } else { format!("{prefix}/{name}") };
                let sub = |e: Option<RawEntry>| e.filter(|e| e.tree()).map(|e| e.oid);
                if sub(here).is_some() || sub(there).is_some() {
                    todo.push((sub(here), sub(there), path.clone()));
                }
                out.push(path);
                if out.len() > crate::graph::MAX_PATHS {
                    return Ok(None);
                }
            }
        }
        Ok(Some(out))
    }

    /// The view of git file `id` at `path`, written from `bodies` the first
    /// time, with the path's last version as its likely delta base.
    fn file(&mut self, id: &Oid, alias: &'static str, bodies: &mut Bodies, path: String) -> Result<View> {
        let v = match self.files.get(&(*id, alias)) {
            Some(v) => v.clone(),
            None => {
                let body = bodies
                    .remove(&(*id, alias))
                    .ok_or_else(|| Error::Io(format!("git blob {} not fetched", hex(id))))?;
                let paired = self.file_pair.remove(id).and_then(|q| {
                    ["raw", "exec", "symlink"]
                        .iter()
                        .find_map(|a| self.files.get(&(q, *a)).map(|v| v.key().to_string()))
                });
                let like = paired.or_else(|| self.last_at.get(&path).cloned());
                let v = self.put(alias, body, false, like)?;
                self.files.insert((*id, alias), v.clone());
                self.n.files += 1;
                v
            }
        };
        self.last_at.insert(path, v.key().to_string());
        Ok(v)
    }

    /// The `tree` view of git tree `id`, whose contents (and those of every
    /// tree under it not converted yet) [`Importer::fetch`] left in `raw`.
    /// A leaf whose git entries (name, mode, id) were met before is not
    /// encoded again: in most new trees only one leaf changed.
    fn tree(&mut self, id: &Oid, raw: &OidMap<Oid, Vec<u8>>, bodies: &mut Bodies, path: &str) -> Result<View> {
        if let Some(v) = self.trees.get(id) {
            return Ok(v.clone());
        }
        let body = raw.get(id).ok_or_else(|| Error::Io(format!("git tree {} not fetched", hex(id))))?;
        let mut es: Vec<(&str, Option<&'static str>, Oid)> = Vec::new();
        for e in entries(id, body)? {
            match e {
                Entry::Tree(name, c) => es.push((name, None, c)),
                Entry::File(name, c, alias) => es.push((name, Some(alias), c)),
                Entry::Skipped => self.n.skipped += 1,
            }
        }
        // Git sorts directories as if their names ended in '/'; dir sorts by name.
        es.sort_by(|a, b| a.0.cmp(b.0));
        if let Some(bad) = es.iter().find(|e| !crate::valid_name(e.0)) {
            return Err(Error::Io(format!("git tree {}: name {:?} is not allowed", hex(id), bad.0)));
        }
        if es.windows(2).any(|w| w[0].0 == w[1].0) {
            return Err(Error::Io(format!("git tree {}: a name occurs twice", hex(id))));
        }
        let format = by_alias("tree").unwrap().id();
        if es.is_empty() {
            let (top, blobs) = TREE.blobs(self.alg, &[]);
            for (k, b) in blobs {
                if self.chunks.insert(k) {
                    self.put("tree", b, true, None)?;
                }
            }
            let v = View { format: format.to_string(), key: top };
            self.trees.insert(*id, v.clone());
            self.n.trees += 1;
            return Ok(v);
        }
        let old: Option<DirBlobs> =
            self.tree_pair.remove(id).and_then(|q| self.dir_blobs.get(&q)).or_else(|| self.last_dir.get(path)).cloned();
        let mut leaves = Vec::new();
        let mut start = 0;
        let b3 = cas_core::algo::lookup("blake3").unwrap();
        let mut fp_buf = Vec::new();
        for (end, h) in crate::dir::tree_cut(es.iter().map(|e| e.0)) {
            let part = &es[start..=end];
            start = end + 1;
            fp_buf.clear();
            for (name, alias, c) in part {
                fp_buf.extend_from_slice(&(name.len() as u32).to_le_bytes());
                fp_buf.extend_from_slice(name.as_bytes());
                fp_buf.extend_from_slice(alias.unwrap_or("tree").as_bytes());
                fp_buf.push(0);
                fp_buf.extend_from_slice(c);
            }
            let fp: [u8; 32] = b3.hash(&fp_buf)[..32].try_into().unwrap();
            let key = match self.leaves.get(&fp) {
                Some(k) => k.clone(),
                None => {
                    let mut items = Vec::with_capacity(part.len());
                    for (name, alias, c) in part {
                        let at = if path.is_empty() { name.to_string() } else { format!("{path}/{name}") };
                        let child = match alias {
                            None => self.tree(c, raw, bodies, &at)?,
                            Some(a) => self.file(c, a, bodies, at)?,
                        };
                        items.push(Item { name: name.to_string(), child });
                    }
                    let bytes = crate::dir::tree_leaf(&items);
                    // Like the leaf of this path's last version that covered
                    // the same names, as gc pairs them.
                    let last = part.last().unwrap().0;
                    let like = old.as_ref().and_then(|(ls, _)| {
                        ls.get(ls.partition_point(|(l, _)| l.as_str() < last).min(ls.len().saturating_sub(1)))
                            .map(|l| l.1.clone())
                    });
                    let k = self.put("tree", bytes, true, like)?.key;
                    self.chunks.insert(k.clone());
                    self.leaves.insert(fp, k.clone());
                    k
                }
            };
            leaves.push((part.last().unwrap().0.to_string(), key, h));
        }
        let mut blobs = Vec::new();
        let leaf_list: Vec<(String, String)> = leaves.iter().map(|(l, k, _)| (l.clone(), k.clone())).collect();
        let top = crate::dir::tree_upper(self.alg, leaves, &mut blobs);
        let old_upper = old.map(|(_, u)| u).unwrap_or_default();
        let mut upper = Vec::with_capacity(blobs.len());
        for (i, (k, b)) in blobs.into_iter().enumerate() {
            upper.push(k.clone());
            if self.chunks.insert(k) {
                let like = old_upper.get(i.min(old_upper.len().saturating_sub(1))).cloned();
                self.put("tree", b, true, like)?;
            }
        }
        self.dir_blobs.insert(*id, (leaf_list.clone(), upper.clone()));
        self.last_dir.insert(path.to_string(), (leaf_list, upper));
        let v = View { format: format.to_string(), key: top };
        self.trees.insert(*id, v.clone());
        self.n.trees += 1;
        Ok(v)
    }
}

/// Import every commit reachable from `rev` in `repo` as a new history.
pub fn import(store: &dyn Store, alg: &'static Algorithm, repo: &FsPath, rev: &str) -> Result<(History, Imported)> {
    import_with_filters(store, alg, repo, rev).map(|(h, n, _)| (h, n))
}

/// [`import`], with each node's changed-path filter against its first git
/// parent (the history graph's, spec/functions/graph.md), from the trees
/// the import reads anyway.
pub fn import_with_filters(
    store: &dyn Store,
    alg: &'static Algorithm,
    repo: &FsPath,
    rev: &str,
) -> Result<(History, Imported, HashMap<NodeId, crate::graph::Filter>)> {
    // git re-encodes messages that declare an encoding; a message that is
    // not UTF-8 and says nothing (old latin-1 commits) is kept lossily.
    let log =
        git(repo, &["log", "--encoding=UTF-8", "--topo-order", "--reverse", "--format=%H%x1f%P%x1f%T%x1f%B%x1e", rev])?;
    let log = String::from_utf8_lossy(&log);
    let mut im = Importer {
        store,
        alg,
        cat: CatFile::start(repo)?,
        trees: OidMap::default(),
        chunks: Default::default(),
        written: Default::default(),
        files: OidMap::default(),
        leaves: OidMap::default(),
        last_at: HashMap::new(),
        last_dir: HashMap::new(),
        tree_pair: OidMap::default(),
        file_pair: OidMap::default(),
        dir_blobs: OidMap::default(),
        old_raw: OidMap::default(),
        extra: OidMap::default(),
        n: Imported::default(),
        pending: Vec::new(),
        pending_bytes: 0,
    };
    let mut h = History::default();
    let mut nodes: BTreeMap<String, NodeId> = BTreeMap::new();
    let recs: Vec<&str> = log.split('\x1e').map(|r| r.trim_start_matches('\n')).filter(|r| !r.is_empty()).collect();
    // Commits in chunks: the trees and files of a chunk are fetched
    // together, a level of directories per round trip.
    let mut root_of: HashMap<String, Oid> = HashMap::new();
    let mut filters: HashMap<NodeId, crate::graph::Filter> = HashMap::new();
    for chunk in recs.chunks(256) {
        let mut parsed = Vec::with_capacity(chunk.len());
        for rec in chunk {
            let f: Vec<&str> = rec.splitn(4, '\x1f').collect();
            let [commit, parents, tree, message] = f[..] else {
                return Err(Error::Io(format!("bad git log record {rec:?}")));
            };
            parsed.push((commit, parents, oid(tree)?, message));
        }
        let (mut raw, mut bodies) = (OidMap::default(), Bodies::default());
        let mut roots = Vec::with_capacity(parsed.len());
        for (commit, parents, tree, _) in &parsed {
            let qs = parents.split_whitespace().filter_map(|p| root_of.get(p)).copied().collect();
            roots.push((*tree, qs));
            root_of.insert(commit.to_string(), *tree);
        }
        im.fetch(&roots, &mut raw, &mut bodies)?;
        for (commit, parents, tree, message) in parsed {
            let version = im.tree(&tree, &raw, &mut bodies, "")?;
            let message = message.trim_end();
            let step = Step { changelog: (!message.is_empty()).then(|| message.to_string()), compatible: false };
            let mut after = BTreeMap::new();
            for p in parents.split_whitespace() {
                let pred = nodes.get(p).ok_or_else(|| Error::Io(format!("parent {p} of {commit} not imported")))?;
                after.insert(pred.clone(), step.clone());
            }
            let doc = if parents.is_empty() { step.changelog.clone() } else { None };
            let id = h.add(&version, after, doc)?;
            let filter = match parents.split_whitespace().next() {
                None => crate::graph::Filter::Root,
                Some(p) => match im.changed(&raw, tree, root_of[p])? {
                    Some(paths) => {
                        let paths: Vec<Path> = paths.into_iter().map(Path::new).collect::<Result<_>>()?;
                        crate::graph::Filter::of(&paths)
                    }
                    None => crate::graph::Filter::Full,
                },
            };
            filters.insert(id.clone(), filter);
            nodes.insert(commit.to_string(), id);
            im.n.commits += 1;
        }
        im.old_raw = raw;
        im.extra.clear();
    }
    // Tags that point (through annotated tags) at imported commits.
    let refs = String::from_utf8(git(repo, &["show-ref", "--tags", "-d"]).unwrap_or_default()).unwrap_or_default();
    for line in refs.lines() {
        let Some((id, name)) = line.split_once(' ') else { continue };
        let name = name.trim_start_matches("refs/tags/");
        let (name, peeled) = match name.strip_suffix("^{}") {
            Some(n) => (n, true),
            None => (name, false),
        };
        if let Some(node) = nodes.get(id)
            && (peeled || !h.tags.contains_key(name))
        {
            h.tags.insert(name.to_string(), node.clone());
        }
    }
    im.flush()?;
    Ok((h, im.n, filters))
}
