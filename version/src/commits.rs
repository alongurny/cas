//! Histories of commits (spec/formats/commit.md). A commit names a
//! version, its author, the time, the notes on the version, and each parent
//! with the notes on the step from it. A commit's key is its id; a history
//! is a variable holding a view of its newest commit.
//!
//! Notes are an `annotation` tree that mirrors the directory tree, with a
//! `lines` blob for each noted file and `runs` for its lines, each note a
//! `note` blob stored once.

use crate::history::{History, NodeId, Step};
use crate::stepnotes::{PathNotes, Runs, StepNotes};
use crate::typed::{NONE, RecV, Value, by_type, check_text, key_bytes, key_text};
use crate::{Error, Result, View, by_alias, malformed, read_blob};
use cas_core::algo::Algorithm;
use cas_core::store::Put;
use cas_core::{Expect, Store};
use std::collections::{BTreeMap, BinaryHeap, HashMap};

/// One commit. Every field but `time` is a blake3 key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    /// The version: a `directory`.
    pub version: String,
    pub author: String,
    /// Notes on the version as it is: an `annotation`.
    pub notes: Option<String>,
    /// Seconds since 1970.
    pub time: u64,
    /// Each parent commit, with the `annotation` of the step from it.
    pub parents: BTreeMap<String, Option<String>>,
}

fn bytes_of(key: &str) -> Result<[u8; 32]> {
    key_bytes(key).ok_or_else(|| Error::BadKey(key.to_string()))
}

fn maybe(key: &Option<String>) -> Result<[u8; 32]> {
    key.as_deref().map_or(Ok(NONE), bytes_of)
}

fn present(key: &[u8; 32]) -> Option<String> {
    (*key != NONE).then(|| key_text(key))
}

/// A view of `key` in the format `alias`.
pub fn view(alias: &str, key: &str) -> View {
    View { format: by_alias(alias).expect("a known format").id().to_string(), key: key.to_string() }
}

impl Commit {
    pub fn encode(&self) -> Result<Vec<u8>> {
        let head = RecV {
            keys: vec![bytes_of(&self.version)?, bytes_of(&self.author)?, maybe(&self.notes)?],
            ints: vec![self.time],
            ..Default::default()
        };
        let mut list = Vec::new();
        for (p, step) in &self.parents {
            list.push(RecV { keys: vec![bytes_of(p)?, maybe(step)?], ..Default::default() });
        }
        Ok(by_type("commit").unwrap().print(&Value { head: Some(head), list }))
    }

    pub fn decode(b: &[u8]) -> std::result::Result<Commit, String> {
        let t = by_type("commit").unwrap();
        let v = t.parse(b)?;
        if t.print(&v) != b {
            return Err("not the one encoding of its value".into());
        }
        let h = v.head.ok_or("a commit has a head")?;
        Ok(Commit {
            version: key_text(&h.keys[0]),
            author: key_text(&h.keys[1]),
            notes: present(&h.keys[2]),
            time: h.ints[0],
            parents: v.list.iter().map(|r| (key_text(&r.keys[0]), present(&r.keys[1]))).collect(),
        })
    }

    /// The version as a view.
    pub fn version(&self) -> View {
        view("directory", &self.version)
    }
}

pub fn read(store: &dyn Store, key: &str) -> Result<Commit> {
    Commit::decode(&read_blob(store, key)?).map_err(|e| malformed(key, e))
}

pub fn write(store: &dyn Store, alg: &'static Algorithm, c: &Commit) -> Result<String> {
    crate::write_blob(store, alg, &c.encode()?)
}

/// Write `text` as a `note` or an `author`; its key.
pub fn put_text(store: &dyn Store, alg: &'static Algorithm, alias: &str, text: &str) -> Result<String> {
    Ok(crate::put(store, alg, alias, text.as_bytes())?.key)
}

/// The text of a `note` or an `author`.
pub fn text(store: &dyn Store, key: &str) -> Result<String> {
    let b = read_blob(store, key)?;
    check_text(&b).map_err(|e| malformed(key, e))?;
    Ok(String::from_utf8(b).expect("checked text is UTF-8"))
}

/// A text as one note: its non-blank lines joined by ` / `, control
/// characters as spaces.
pub fn one_line(s: &str) -> String {
    let clean = |l: &str| -> String { l.trim().chars().map(|c| if c.is_control() { ' ' } else { c }).collect() };
    s.lines().map(clean).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" / ")
}

// Histories: variables.

/// The newest commit of history `name`: `None` when the variable is unset,
/// an error when it holds something other than a commit.
pub fn tip(store: &dyn Store, name: &str) -> Result<Option<String>> {
    match crate::history::head(store, name)? {
        None => Ok(None),
        Some(v) if is_commit(&v) => Ok(Some(v.key)),
        Some(v) => Err(Error::UnknownFormat(format!("history {name} holds {v}, not a commit"))),
    }
}

pub fn is_commit(v: &View) -> bool {
    v.known_format().is_some_and(|f| f.alias == "commit")
}

/// Move history `name` from `old` to `new`, only if it still holds `old`.
pub fn advance(store: &dyn Store, name: &str, old: Option<&str>, new: &str) -> Result<()> {
    let expect = match old {
        None => Expect::Absent,
        Some(o) => Expect::Value(view("commit", o).to_string().into_bytes()),
    };
    store.write(name, view("commit", new).to_string().as_bytes(), &expect)?;
    Ok(())
}

/// Every commit `tip` reaches, newest first: each before its parents, by
/// time where the graph leaves a choice.
pub fn log(store: &dyn Store, tip: &str) -> Result<Vec<(String, Commit)>> {
    let mut all: HashMap<String, Commit> = HashMap::new();
    let mut wave = vec![tip.to_string()];
    while !wave.is_empty() {
        let mut next = Vec::new();
        for (k, r) in wave.iter().zip(store.read_many(&wave)) {
            let b = r?.ok_or_else(|| Error::Missing(k.clone()))?;
            let c = Commit::decode(&b).map_err(|e| malformed(k, e))?;
            next.extend(c.parents.keys().filter(|p| !all.contains_key(*p)).cloned());
            all.insert(k.clone(), c);
        }
        next.sort();
        next.dedup();
        next.retain(|k| !all.contains_key(k));
        wave = next;
    }
    let mut children: HashMap<&str, usize> = HashMap::new();
    for c in all.values() {
        for p in c.parents.keys() {
            *children.entry(p.as_str()).or_default() += 1;
        }
    }
    let mut ready = BinaryHeap::from([(all[tip].time, tip.to_string())]);
    let mut out = Vec::with_capacity(all.len());
    while let Some((_, k)) = ready.pop() {
        let c = all[&k].clone();
        for p in c.parents.keys() {
            let n = children.get_mut(p.as_str()).expect("counted");
            *n -= 1;
            if *n == 0 {
                ready.push((all[p].time, p.clone()));
            }
        }
        out.push((k, c));
    }
    Ok(out)
}

/// The commit `what` names in the history ending at `tip`: `latest`, a
/// key, or the start of one.
pub fn resolve(store: &dyn Store, tip: &str, what: &str) -> Result<Option<String>> {
    if what == "latest" {
        return Ok(Some(tip.to_string()));
    }
    let digits = what.strip_prefix("blake3:").unwrap_or(what);
    if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Ok(None);
    }
    let want = format!("blake3:{}", digits.to_ascii_lowercase());
    let hits: Vec<String> = log(store, tip)?.into_iter().map(|(k, _)| k).filter(|k| k.starts_with(&want)).collect();
    Ok(if hits.len() == 1 { hits.into_iter().next() } else { None })
}

// Notes.

/// Notes on a version or on a step: one on the whole, and one for each
/// noted path. A directory's entry has a note only; a file's may have runs
/// over its lines before the step (`old`) and after it (`new`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Notes {
    pub note: String,
    pub paths: BTreeMap<String, PathNotes>,
}

impl Notes {
    pub fn is_empty(&self) -> bool {
        self.note.is_empty() && self.paths.is_empty()
    }
}

enum Tree {
    Dir(String, BTreeMap<String, Tree>),
    File(PathNotes),
}

fn has_runs(n: &PathNotes) -> bool {
    n.old.is_some() || n.new.is_some()
}

struct Batch<'a> {
    alg: &'static Algorithm,
    store: &'a dyn Store,
    puts: Vec<(String, Vec<u8>)>,
    notes: HashMap<String, [u8; 32]>,
}

impl Batch<'_> {
    fn put(&mut self, alias: &str, v: &Value) -> [u8; 32] {
        let b = by_type(alias).unwrap().print(v);
        let k = self.alg.key(&b);
        let out = key_bytes(&k).expect("blake3");
        self.puts.push((k, b));
        out
    }

    fn note(&mut self, text: &str) -> Result<[u8; 32]> {
        if text.is_empty() {
            return Ok(NONE);
        }
        if let Some(k) = self.notes.get(text) {
            return Ok(*k);
        }
        check_text(text.as_bytes()).map_err(|e| malformed("a note", e))?;
        let k = self.alg.key(text.as_bytes());
        let out = key_bytes(&k).expect("blake3");
        self.puts.push((k, text.as_bytes().to_vec()));
        self.notes.insert(text.to_string(), out);
        Ok(out)
    }

    fn runs(&mut self, runs: &Option<Runs>) -> Result<[u8; 32]> {
        let Some(runs) = runs else { return Ok(NONE) };
        if runs.is_empty() {
            return Ok(NONE);
        }
        let mut list = Vec::with_capacity(runs.len());
        for (n, note) in runs {
            list.push(RecV { keys: vec![self.note(note)?], ints: vec![*n as u64], ..Default::default() });
        }
        Ok(self.put("runs", &Value { head: None, list }))
    }

    /// The blob of `t`, and its tag in an annotation's entry; `None` when
    /// it holds nothing.
    fn tree(&mut self, t: &Tree) -> Result<Option<([u8; 32], u8)>> {
        match t {
            Tree::File(n) => {
                let keys = vec![self.note(&n.note)?, self.runs(&n.old)?, self.runs(&n.new)?];
                if keys.iter().all(|k| *k == NONE) {
                    return Ok(None);
                }
                Ok(Some((
                    self.put("lines", &Value { head: Some(RecV { keys, ..Default::default() }), list: vec![] }),
                    2,
                )))
            }
            Tree::Dir(note, kids) => {
                let mut list = Vec::new();
                for (name, kid) in kids {
                    if let Some((key, tag)) = self.tree(kid)? {
                        list.push(RecV {
                            keys: vec![key],
                            kinds: vec![tag],
                            name: name.as_bytes().to_vec(),
                            ..Default::default()
                        });
                    }
                }
                let note = self.note(note)?;
                if note == NONE && list.is_empty() {
                    return Ok(None);
                }
                let head = RecV { keys: vec![note], ..Default::default() };
                Ok(Some((self.put("annotation", &Value { head: Some(head), list }), 1)))
            }
        }
    }

    fn flush(&mut self) -> Result<()> {
        let keys: Vec<String> = self.puts.iter().map(|p| p.0.clone()).collect();
        let have = self.store.has_many(&keys)?;
        let puts: Vec<Put> =
            self.puts.drain(..).zip(have).filter(|(_, h)| !h).map(|((k, b), _)| Put::blob(k, b)).collect();
        self.store.write_many(&puts).map_err(|(_, e)| Error::Store(e))
    }
}

/// Write `notes` as an annotation; its key, or `None` when there is nothing
/// to say. `is_dir` tells whether a path with a note and no line runs is a
/// directory.
pub fn write_notes(
    store: &dyn Store,
    alg: &'static Algorithm,
    notes: &Notes,
    is_dir: &dyn Fn(&str) -> Result<bool>,
) -> Result<Option<String>> {
    let mut top: BTreeMap<String, Tree> = BTreeMap::new();
    for (path, n) in &notes.paths {
        crate::Path::new(path.as_str())?;
        let names: Vec<&str> = path.split('/').collect();
        let mut at = &mut top;
        for (i, name) in names.iter().enumerate() {
            let last = i + 1 == names.len();
            let both = || malformed("notes", format!("{path} is noted as a file and as a directory"));
            if last {
                match at.get_mut(*name) {
                    Some(Tree::Dir(note, _)) => {
                        if has_runs(n) {
                            return Err(both());
                        }
                        *note = n.note.clone();
                    }
                    Some(Tree::File(_)) => unreachable!("a path is noted once"),
                    None => {
                        let t = if !has_runs(n) && is_dir(path)? {
                            Tree::Dir(n.note.clone(), BTreeMap::new())
                        } else {
                            Tree::File(n.clone())
                        };
                        at.insert(name.to_string(), t);
                    }
                }
                break;
            }
            let slot = at.entry(name.to_string()).or_insert_with(|| Tree::Dir(String::new(), BTreeMap::new()));
            if let Tree::File(f) = slot {
                if has_runs(f) {
                    return Err(both());
                }
                *slot = Tree::Dir(f.note.clone(), BTreeMap::new());
            }
            let Tree::Dir(_, kids) = slot else { unreachable!() };
            at = kids;
        }
    }
    let mut b = Batch { alg, store, puts: Vec::new(), notes: HashMap::new() };
    let out = b.tree(&Tree::Dir(notes.note.clone(), top))?;
    b.flush()?;
    Ok(out.map(|(k, _)| key_text(&k)))
}

fn parse(store: &dyn Store, alias: &str, key: &str) -> Result<Value> {
    let b = read_blob(store, key)?;
    let t = by_type(alias).unwrap();
    let v = t.parse(&b).map_err(|e| malformed(key, e))?;
    if t.print(&v) != b {
        return Err(malformed(key, "not the one encoding of its value"));
    }
    Ok(v)
}

fn note_at(store: &dyn Store, key: &[u8; 32]) -> Result<String> {
    if *key == NONE { Ok(String::new()) } else { text(store, &key_text(key)) }
}

fn read_runs(store: &dyn Store, key: &[u8; 32]) -> Result<Option<Runs>> {
    if *key == NONE {
        return Ok(None);
    }
    let v = parse(store, "runs", &key_text(key))?;
    v.list.iter().map(|r| Ok((r.ints[0] as usize, note_at(store, &r.keys[0])?))).collect::<Result<Runs>>().map(Some)
}

fn read_into(store: &dyn Store, key: &str, at: Option<&str>, out: &mut Notes) -> Result<()> {
    let v = parse(store, "annotation", key)?;
    let note = note_at(store, &v.head.as_ref().expect("an annotation has a head").keys[0])?;
    match at {
        None => out.note = note,
        Some(p) if !note.is_empty() => {
            out.paths.insert(p.to_string(), PathNotes { note, old: None, new: None });
        }
        Some(_) => {}
    }
    for e in &v.list {
        let name = std::str::from_utf8(&e.name).expect("a parsed name is valid");
        let path = at.map_or(name.to_string(), |d| format!("{d}/{name}"));
        let child = key_text(&e.keys[0]);
        if e.kinds[0] == 1 {
            read_into(store, &child, Some(&path), out)?;
        } else {
            let l = parse(store, "lines", &child)?;
            let k = &l.head.as_ref().expect("lines have a head").keys;
            let n = PathNotes {
                note: note_at(store, &k[0])?,
                old: read_runs(store, &k[1])?,
                new: read_runs(store, &k[2])?,
            };
            out.paths.insert(path, n);
        }
    }
    Ok(())
}

/// The notes of the annotation at `key`.
pub fn read_notes(store: &dyn Store, key: &str) -> Result<Notes> {
    let mut out = Notes::default();
    read_into(store, key, None, &mut out)?;
    Ok(out)
}

/// The note on the whole of the annotation at `key`, reading only its top.
pub fn top_note(store: &dyn Store, key: &str) -> Result<String> {
    let v = parse(store, "annotation", key)?;
    note_at(store, &v.head.as_ref().expect("an annotation has a head").keys[0])
}

/// What a commit says in one line: the note on a step into it, or else the
/// note on its version.
pub fn line(store: &dyn Store, c: &Commit) -> Result<String> {
    for step in c.parents.values().flatten().chain(c.notes.iter()) {
        let n = top_note(store, step)?;
        if !n.is_empty() {
            return Ok(n);
        }
    }
    Ok(String::new())
}

/// Whether each run list of `notes` covers the lines of its file: `old`
/// runs those of the file in `before`, `new` runs those in `after`.
pub fn fits(store: &dyn Store, notes: &Notes, before: Option<&View>, after: &View) -> Result<()> {
    for (p, n) in &notes.paths {
        for (version, runs, side) in [(before, &n.old, "old"), (Some(after), &n.new, "new")] {
            let Some(runs) = runs else { continue };
            let path = crate::Path::new(p.as_str())?;
            let file = match version {
                Some(v) => crate::object_at(store, v, &path)?.filter(|f| !f.is_dir()),
                None => None,
            };
            let have = match file {
                Some(f) => Some(crate::annotated::lines(&read_blob(store, f.key())?).len()),
                None => None,
            };
            let want: usize = runs.iter().map(|r| r.0).sum();
            if have != Some(want) {
                return Err(malformed("notes", format!("{p}: {side} runs for {want} lines, the file has {have:?}")));
            }
        }
    }
    Ok(())
}

// From the earlier formats.

/// Rewrites versions and histories of the earlier formats (`dir`, `tree`,
/// `history`, `step-notes`, `annotated`) in the formats given as types.
pub struct Converter<'a> {
    store: &'a dyn Store,
    alg: &'static Algorithm,
    dirs: HashMap<String, String>,
}

impl<'a> Converter<'a> {
    pub fn new(store: &'a dyn Store, alg: &'static Algorithm) -> Converter<'a> {
        Converter { store, alg, dirs: HashMap::new() }
    }

    /// The `directory` holding what the directory `v` holds. An `exec`
    /// file becomes `raw`: its bytes, and so its key, stay.
    pub fn directory(&mut self, v: &View) -> Result<String> {
        if v.known_format().is_some_and(|f| f.alias == "directory") {
            return Ok(v.key.clone());
        }
        if let Some(k) = self.dirs.get(&v.key) {
            return Ok(k.clone());
        }
        let mut items = crate::tree::load(self.store, v)?;
        for i in &mut items {
            let alias = i.child.known_format().map_or("?", |f| f.alias);
            i.child = match alias {
                _ if i.child.is_dir() => view("directory", &self.directory(&i.child)?),
                "raw" | "exec" => view("raw", &i.child.key),
                "symlink" => i.child.clone(),
                other => return Err(Error::UnknownFormat(format!("a directory cannot hold {other} ({})", i.name))),
            };
        }
        crate::dir::DIRECTORY.fits(&items).map_err(Error::UnknownFormat)?;
        let (top, blobs) = crate::dir::DIRECTORY.blobs(self.alg, &items);
        let puts: Vec<Put> = blobs.into_iter().map(|(k, b)| Put::blob(k, b)).collect();
        self.store.write_many(&puts).map_err(|(_, e)| Error::Store(e))?;
        self.dirs.insert(v.key.clone(), top.clone());
        Ok(top)
    }

    fn is_dir(&self, versions: &[&View], path: &str) -> Result<bool> {
        let path = crate::Path::new(path)?;
        for v in versions {
            if let Some(o) = crate::object_at(self.store, v, &path)? {
                return Ok(o.is_dir());
            }
        }
        Ok(false)
    }

    /// The annotation of a step of the earlier `history` format.
    fn step(&mut self, before: &View, after: &View, step: &Step) -> Result<Option<String>> {
        let notes = match &step.notes {
            Some(v) if v.known_format().is_some_and(|f| f.alias == "step-notes") => {
                let sn = StepNotes::decode(&read_blob(self.store, v.key())?).map_err(|e| malformed(v.key(), e))?;
                Notes { note: sn.note, paths: sn.paths }
            }
            Some(v) => return Err(Error::UnknownFormat(format!("step notes {v} are not step-notes"))),
            None => Notes { note: step.changelog.as_deref().map(one_line).unwrap_or_default(), paths: BTreeMap::new() },
        };
        write_notes(self.store, self.alg, &notes, &|p| self.is_dir(&[after, before], p))
    }

    /// The commits of the history `h`: each node's commit key. `meta` gives
    /// a node's author and time, which the earlier format did not keep.
    pub fn history(
        &mut self,
        h: &History,
        meta: &dyn Fn(&NodeId) -> (String, u64),
    ) -> Result<BTreeMap<NodeId, String>> {
        let mut out: BTreeMap<NodeId, String> = BTreeMap::new();
        let mut authors: HashMap<String, String> = HashMap::new();
        for (id, node) in crate::history::predecessors_first(h.nodes.clone()) {
            let old = id.version();
            let version = self.directory(&old)?;
            let mut parents = BTreeMap::new();
            for (p, step) in &node.after {
                let parent = out.get(p).ok_or_else(|| Error::Missing(format!("node {p}")))?.clone();
                parents.insert(parent, self.step(&p.version(), &old, step)?);
            }
            let mut notes =
                Notes { note: node.doc.as_deref().map(one_line).unwrap_or_default(), paths: BTreeMap::new() };
            for (path, v) in &node.notes {
                let a = crate::annotated::Annotated::decode(&read_blob(self.store, v.key())?)
                    .map_err(|e| malformed(v.key(), e))?;
                if a.runs.iter().any(|r| !r.1.is_empty()) {
                    notes.paths.insert(path.clone(), PathNotes { note: String::new(), old: None, new: Some(a.runs) });
                }
            }
            let notes = write_notes(self.store, self.alg, &notes, &|p| self.is_dir(&[&old], p))?;
            let (author, time) = meta(&id);
            let author = match authors.get(&author) {
                Some(k) => k.clone(),
                None => {
                    let k = put_text(self.store, self.alg, "author", &one_line(&author))?;
                    authors.insert(author, k.clone());
                    k
                }
            };
            let c = Commit { version, author, notes, time, parents };
            out.insert(id, write(self.store, self.alg, &c)?);
        }
        Ok(out)
    }
}

/// Import every commit reachable from `rev` in the git repository `repo`
/// as commits: the key of the newest, and how many there are. A git
/// commit's message becomes the note on its steps and its subject the note
/// on the lines it changed; its author and committer time are kept.
pub fn import_git(
    store: &dyn Store,
    alg: &'static Algorithm,
    repo: &std::path::Path,
    rev: &str,
) -> Result<(String, crate::git::Imported)> {
    let (h, n, _) = crate::git::import_with_filters(store, alg, repo, rev, true)?;
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["log", "--encoding=UTF-8", "--format=%H%x1f%an <%ae>%x1f%ct", rev])
        .output()?;
    if !out.status.success() {
        return Err(Error::Io(format!("git log: {}", String::from_utf8_lossy(&out.stderr).trim())));
    }
    let mut meta: HashMap<NodeId, (String, u64)> = HashMap::new();
    for l in String::from_utf8_lossy(&out.stdout).lines() {
        let f: Vec<&str> = l.split('\x1f').collect();
        let [commit, author, time] = f[..] else { return Err(Error::Io(format!("bad git log line {l:?}"))) };
        if let Some(id) = n.nodes.get(commit) {
            meta.insert(id.clone(), (author.to_string(), time.parse().unwrap_or(0)));
        }
    }
    let commits = Converter::new(store, alg).history(&h, &|id| meta.get(id).cloned().unwrap_or_default())?;
    let tips = h.tips();
    let tip = match (tips.len(), tips.first()) {
        (1, Some(t)) => commits[t].clone(),
        _ => return Err(Error::Diverged(format!("{rev} (it has {} tips)", tips.len()))),
    };
    Ok((tip, n))
}

// Sync, merge and rebase.

/// Whether `old` is `tip` or one of the commits before it.
pub fn reaches(store: &dyn Store, tip: &str, old: &str) -> Result<bool> {
    Ok(tip == old || log(store, tip)?.iter().any(|(k, _)| k == old))
}

/// [`crate::sync::push`] for a history of commits: `to` in `dst` moves to
/// the commit `from` holds in `src` when it is unset or holds a commit
/// that one reaches.
pub fn push(
    src: &dyn Store,
    dst: &dyn Store,
    from: &str,
    to: &str,
) -> Result<(crate::sync::Outcome, crate::sync::Copied)> {
    use crate::sync::{Copied, Outcome};
    let head = tip(src, from)?.ok_or_else(|| Error::Missing(format!("history {from} in the source")))?;
    let theirs = tip(dst, to)?;
    if theirs.as_deref() == Some(head.as_str()) {
        return Ok((Outcome::UpToDate, Copied::default()));
    }
    if let Some(t) = &theirs {
        // Their commit is in the source when ours comes after it.
        if !src.has_many(std::slice::from_ref(t))?[0] || !reaches(src, &head, t)? {
            return Err(Error::Diverged(to.to_string()));
        }
    }
    let copied = crate::sync::copy(src, dst, &view("commit", &head))?;
    match advance(dst, to, theirs.as_deref(), &head) {
        Ok(()) => Ok((if theirs.is_some() { Outcome::FastForward } else { Outcome::Created }, copied)),
        Err(Error::Store(cas_core::Error::Conflict)) => Err(Error::Diverged(to.to_string())),
        Err(e) => Err(e),
    }
}

/// The newest commit both `a` and `b` reach, if any.
pub fn merge_base(store: &dyn Store, a: &str, b: &str) -> Result<Option<String>> {
    let theirs: std::collections::HashSet<String> = log(store, b)?.into_iter().map(|(k, _)| k).collect();
    // Newest first: the first of ours that is also theirs.
    Ok(log(store, a)?.into_iter().map(|(k, _)| k).find(|k| theirs.contains(k)))
}

/// What a merge or a rebase did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    UpToDate,
    /// The history moved to a commit that was already there.
    FastForward(String),
    /// A new commit (the last of a rebase).
    Done(String),
    /// Nothing recorded: the conflicts.
    Conflicts(crate::merge::Merged),
}

/// Merge the commit `theirs` into the commit `ours`: the commit to move the
/// history to. The merge commit has both as parents, with no step notes.
pub fn merge(
    store: &dyn Store,
    alg: &'static Algorithm,
    ours: &str,
    theirs: &str,
    author: &str,
    time: u64,
) -> Result<Outcome> {
    if reaches(store, ours, theirs)? {
        return Ok(Outcome::UpToDate);
    }
    if reaches(store, theirs, ours)? {
        return Ok(Outcome::FastForward(theirs.to_string()));
    }
    let base = merge_base(store, ours, theirs)?
        .ok_or_else(|| Error::Diverged("the two histories share no commit".to_string()))?;
    let (b, o, t) = (read(store, &base)?, read(store, ours)?, read(store, theirs)?);
    let m = crate::merge::merge_versions(store, alg, &b.version(), &o.version(), &t.version())?;
    if !m.conflicts.is_empty() {
        return Ok(Outcome::Conflicts(m));
    }
    let c = Commit {
        version: m.version.key.clone(),
        author: put_text(store, alg, "author", &one_line(author))?,
        notes: None,
        time,
        parents: BTreeMap::from([(ours.to_string(), None), (theirs.to_string(), None)]),
    };
    Ok(Outcome::Done(write(store, alg, &c)?))
}

/// Replay the commits `ours` has and `onto` lacks on top of `onto`, oldest
/// first. They must be a chain (no merges among them). A step keeps its
/// annotation where its line runs still fit the files, else its notes
/// without the runs. Authors and times stay.
pub fn rebase(store: &dyn Store, alg: &'static Algorithm, ours: &str, onto: &str) -> Result<Outcome> {
    if reaches(store, onto, ours)? {
        return Ok(Outcome::FastForward(onto.to_string()));
    }
    if reaches(store, ours, onto)? {
        return Ok(Outcome::UpToDate);
    }
    let base = merge_base(store, ours, onto)?
        .ok_or_else(|| Error::Diverged("the two histories share no commit".to_string()))?;
    let mut chain = Vec::new();
    let mut at = ours.to_string();
    while at != base {
        let c = read(store, &at)?;
        let mut parents = c.parents.keys();
        let (Some(p), None) = (parents.next().cloned(), parents.next()) else {
            return Err(Error::Diverged("a merge is among the commits to replay; merge instead".to_string()));
        };
        chain.push((c, p.clone()));
        at = p;
    }
    let mut tip = onto.to_string();
    for (c, parent) in chain.into_iter().rev() {
        let (was, new_parent) = (read(store, &parent)?, read(store, &tip)?);
        let m = crate::merge::merge_versions(store, alg, &was.version(), &new_parent.version(), &c.version())?;
        if !m.conflicts.is_empty() {
            return Ok(Outcome::Conflicts(m));
        }
        let step = match &c.parents[&parent] {
            None => None,
            Some(k) => {
                let mut n = read_notes(store, k)?;
                if fits(store, &n, Some(&new_parent.version()), &m.version).is_ok() {
                    Some(k.clone())
                } else {
                    for p in n.paths.values_mut() {
                        (p.old, p.new) = (None, None);
                    }
                    n.paths.retain(|_, p| !p.note.is_empty());
                    let is_dir = |p: &str| -> Result<bool> {
                        Ok(crate::object_at(store, &m.version, &crate::Path::new(p)?)?.is_some_and(|v| v.is_dir()))
                    };
                    write_notes(store, alg, &n, &is_dir)?
                }
            }
        };
        let new = Commit { version: m.version.key.clone(), parents: BTreeMap::from([(tip.clone(), step)]), ..c };
        tip = write(store, alg, &new)?;
    }
    Ok(Outcome::Done(tip))
}

/// The commits of the history ending at `tip` whose version differs from
/// every parent's at `path` (a first commit: has the path), newest first.
pub fn path_log(store: &dyn Store, tip: &str, path: &crate::Path) -> Result<Vec<(String, Commit)>> {
    let all = log(store, tip)?;
    let mut memo = crate::dir::FindMemo::new();
    let mut at: HashMap<String, Option<View>> = HashMap::new();
    for (k, c) in &all {
        at.insert(k.clone(), crate::tree::object_at_memo(store, &c.version(), path, &mut memo)?);
    }
    Ok(all
        .into_iter()
        .filter(|(k, c)| if c.parents.is_empty() { at[k].is_some() } else { c.parents.keys().all(|p| at[p] != at[k]) })
        .collect())
}

/// For each path, the newest commit of the history ending at `tip` whose
/// version differs from every parent's there (a first commit: has the
/// path), or none.
pub fn last_changes(store: &dyn Store, tip: &str, paths: &[crate::Path]) -> Result<Vec<Option<String>>> {
    let all = log(store, tip)?;
    let index: HashMap<&str, usize> = all.iter().enumerate().map(|(i, (k, _))| (k.as_str(), i)).collect();
    let mut memo = crate::dir::FindMemo::new();
    let mut found = Vec::new();
    for path in paths {
        // What each commit has at the path, read only as far as the walk goes.
        let mut at: Vec<Option<Option<View>>> = vec![None; all.len()];
        let mut see = |i: usize| -> Result<Option<View>> {
            if at[i].is_none() {
                at[i] = Some(crate::tree::object_at_memo(store, &all[i].1.version(), path, &mut memo)?);
            }
            Ok(at[i].clone().unwrap())
        };
        let mut last = None;
        for (i, (k, c)) in all.iter().enumerate() {
            let here = see(i)?;
            let mut changed = !c.parents.is_empty() || here.is_some();
            for p in c.parents.keys() {
                changed = changed && see(index[p.as_str()])? != here;
            }
            if changed {
                last = Some(k.clone());
                break;
            }
        }
        found.push(last);
    }
    Ok(found)
}

/// Remove history `name`, only if it still holds `old`.
pub fn drop(store: &dyn Store, name: &str, old: &str) -> Result<()> {
    store.remove(name, &Expect::Value(view("commit", old).to_string().into_bytes()))?;
    Ok(())
}
