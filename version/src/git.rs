//! Import a git repository's history (spec/git-import.md): git trees become
//! `dir` objects, files become `raw`, `exec` or `symlink` objects, commits
//! become history nodes, commit messages become step changelogs, and git
//! tags become history tags. Runs the `git` command; reads only.

use crate::dir::{Item, TREE};
use crate::history::{History, NodeId, Step};
use crate::{Error, Result, View, by_alias};
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
    files: OidMap<(Oid, &'static str), View>,
    /// Leaf keys by a hash of the git entries they hold.
    leaves: OidMap<[u8; 32], String>,
    n: Imported,
}

impl Importer<'_> {
    /// [`crate::put`], batched: the write happens at the next flush.
    /// `checked`: the bytes were made here and are valid by construction.
    fn put(&mut self, alias: &str, bytes: Vec<u8>, checked: bool) -> Result<View> {
        let f = by_alias(alias).ok_or_else(|| Error::UnknownFormat(alias.to_string()))?;
        let key = self.alg.key(&bytes);
        if checked {
            debug_assert!(f.check(&key, &bytes).is_ok());
        } else {
            f.check(&key, &bytes)?;
        }
        let view = View { format: f.id().to_string(), key: key.clone() };
        self.pending_bytes += bytes.len();
        self.pending.push(Put::blob(key, bytes));
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
    /// one level at a time (one round trip to git each), writing the files.
    /// The trees' contents are left in `raw`.
    fn fetch(&mut self, roots: &[Oid], raw: &mut OidMap<Oid, Vec<u8>>) -> Result<()> {
        let mut want: Vec<Oid> =
            roots.iter().filter(|r| !self.trees.contains_key(*r) && !raw.contains_key(*r)).copied().collect();
        want.sort_unstable();
        want.dedup();
        let mut files: Vec<(Oid, &'static str)> = Vec::new();
        while !want.is_empty() || !files.is_empty() {
            let ids: Vec<Oid> = want.iter().copied().chain(files.iter().map(|(o, _)| *o)).collect();
            let mut got = self.cat.get_many(&ids)?.into_iter();
            let mut next = Vec::new();
            let mut queued = OidSet::default();
            let mut new_files = Vec::new();
            let mut seen = OidSet::default();
            for id in &want {
                let body = got.next().unwrap();
                for e in entries(id, &body)? {
                    match e {
                        Entry::Tree(_, c)
                            if !self.trees.contains_key(&c) && !raw.contains_key(&c) && queued.insert(c) =>
                        {
                            next.push(c)
                        }
                        Entry::File(_, c, alias)
                            if !self.files.contains_key(&(c, alias)) && seen.insert((c, alias)) =>
                        {
                            new_files.push((c, alias))
                        }
                        _ => {}
                    }
                }
                raw.insert(*id, body);
            }
            for (id, alias) in std::mem::take(&mut files) {
                let body = got.next().unwrap();
                if !self.files.contains_key(&(id, alias)) {
                    let v = self.put(alias, body, false)?;
                    self.files.insert((id, alias), v);
                    self.n.files += 1;
                }
            }
            files = new_files;
            want = next;
        }
        Ok(())
    }

    /// The `tree` view of git tree `id`, whose contents (and those of every
    /// tree under it not converted yet) [`Importer::fetch`] left in `raw`.
    /// A leaf whose git entries (name, mode, id) were met before is not
    /// encoded again: in most new trees only one leaf changed.
    fn tree(&mut self, id: &Oid, raw: &OidMap<Oid, Vec<u8>>) -> Result<View> {
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
                    self.put("tree", b, true)?;
                }
            }
            let v = View { format: format.to_string(), key: top };
            self.trees.insert(*id, v.clone());
            self.n.trees += 1;
            return Ok(v);
        }
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
                        let child = match alias {
                            None => self.tree(c, raw)?,
                            Some(a) => self.files[&(*c, *a)].clone(),
                        };
                        items.push(Item { name: name.to_string(), child });
                    }
                    let bytes = crate::dir::tree_leaf(&items);
                    let k = self.put("tree", bytes, true)?.key;
                    self.chunks.insert(k.clone());
                    self.leaves.insert(fp, k.clone());
                    k
                }
            };
            leaves.push((part.last().unwrap().0.to_string(), key, h));
        }
        let mut blobs = Vec::new();
        let top = crate::dir::tree_upper(self.alg, leaves, &mut blobs);
        for (k, b) in blobs {
            if self.chunks.insert(k) {
                self.put("tree", b, true)?;
            }
        }
        let v = View { format: format.to_string(), key: top };
        self.trees.insert(*id, v.clone());
        self.n.trees += 1;
        Ok(v)
    }
}

/// Import every commit reachable from `rev` in `repo` as a new history.
pub fn import(store: &dyn Store, alg: &'static Algorithm, repo: &FsPath, rev: &str) -> Result<(History, Imported)> {
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
        files: OidMap::default(),
        leaves: OidMap::default(),
        n: Imported::default(),
        pending: Vec::new(),
        pending_bytes: 0,
    };
    let mut h = History::default();
    let mut nodes: BTreeMap<String, NodeId> = BTreeMap::new();
    let recs: Vec<&str> = log.split('\x1e').map(|r| r.trim_start_matches('\n')).filter(|r| !r.is_empty()).collect();
    // Commits in chunks: the trees and files of a chunk are fetched
    // together, a level of directories per round trip.
    for chunk in recs.chunks(256) {
        let mut parsed = Vec::with_capacity(chunk.len());
        for rec in chunk {
            let f: Vec<&str> = rec.splitn(4, '\x1f').collect();
            let [commit, parents, tree, message] = f[..] else {
                return Err(Error::Io(format!("bad git log record {rec:?}")));
            };
            parsed.push((commit, parents, oid(tree)?, message));
        }
        let mut raw = OidMap::default();
        im.fetch(&parsed.iter().map(|p| p.2).collect::<Vec<_>>(), &mut raw)?;
        for (commit, parents, tree, message) in parsed {
            let version = im.tree(&tree, &raw)?;
            let message = message.trim_end();
            let step = Step { changelog: (!message.is_empty()).then(|| message.to_string()), compatible: false };
            let mut after = BTreeMap::new();
            for p in parents.split_whitespace() {
                let pred = nodes.get(p).ok_or_else(|| Error::Io(format!("parent {p} of {commit} not imported")))?;
                after.insert(pred.clone(), step.clone());
            }
            let doc = if parents.is_empty() { step.changelog.clone() } else { None };
            nodes.insert(commit.to_string(), h.add(&version, after, doc)?);
            im.n.commits += 1;
        }
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
    Ok((h, im.n))
}
