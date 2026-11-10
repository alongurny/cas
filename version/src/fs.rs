//! Moving versions to and from a local directory (Unix): import a directory
//! as a version, and check out a version, only the part under a prefix, or
//! a sparse set of paths; compare, reset and clean a working directory.

use crate::formats::{Format, git_blob_contents};
use crate::index::{Index, Stat};
use crate::{Edit, Error, Node, Path, Result, View, build, put, read_blob, walk};
use cas_core::Store;
use cas_core::algo::Algorithm;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path as FsPath;

/// Write every file under `dir` as a blob (`raw`, `exec` or `symlink`) and
/// build a version of them, keeping empty directories. Directories are in
/// `format`. Names must be valid UTF-8 path names.
/// The stat cache file ([`index::FILE`]) and the sparse-checkout file
/// ([`SPARSE`]) at the top are never imported.
pub fn import(store: &dyn Store, alg: &'static Algorithm, format: &'static Format, dir: &FsPath) -> Result<View> {
    import_indexed(store, alg, format, dir, None)
}

/// [`import`] with the stat cache of `dir` for the store named `scope`
/// (`None`: no cache): files whose stat data is unchanged are not read or
/// hashed again. The cache is rewritten afterwards.
pub fn import_indexed(
    store: &dyn Store,
    alg: &'static Algorithm,
    format: &'static Format,
    dir: &FsPath,
    scope: Option<&str>,
) -> Result<View> {
    let old = scope.map(|s| Index::load(dir, s));
    let mut new = scope.map(Index::new);
    // With a watcher, the token comes first: whatever changes while the
    // directory is looked at is logged after it.
    let token = scope.and_then(|_| crate::watch::token(dir));
    let changed = match (&old, &token) {
        (Some(o), Some(now)) => o.watch.as_ref().and_then(|since| crate::watch::changed(dir, since, now)),
        _ => None,
    };
    if let Some(n) = new.as_mut() {
        n.watch = token;
    }
    // The version imported last time, when the watcher tells what changed
    // since and it is still in the store (with everything under it, by the
    // closure invariant): only the changed paths are edited into it.
    let base = match (&changed, &old) {
        (Some(c), Some(o)) => o.root.clone().filter(|r| r.format() == format.id()).map(|r| (c, o, r)),
        _ => None,
    };
    let base = match base {
        Some(b) if store.has_many(std::slice::from_ref(&b.2.key().to_string()))?[0] => Some(b),
        _ => None,
    };
    let mut entries = BTreeMap::new();
    let mut reused = Vec::new();
    let mut scan = Scan { store, alg, old: old.as_ref(), new: new.as_mut(), reused: &mut reused };
    let mut edits = None;
    match &base {
        Some((changed, o, _)) => edits = Some(scan.watched(dir, o, changed, &mut entries)?),
        None => scan.dir(dir, None, &mut entries)?,
    }
    // A reused view must still be in the store (the cache may be older than
    // a collection, or the store a copy): one batched check, and whatever
    // is gone is hashed again.
    let keys: Vec<String> = reused
        .iter()
        .map(|(p, _): &(Path, std::path::PathBuf)| match &entries[p] {
            Node::File(v) => v.key().to_string(),
            Node::Dir => unreachable!(),
        })
        .collect();
    for ((path, file), present) in reused.into_iter().zip(store.has_many(&keys)?) {
        if !present {
            let meta = fs::symlink_metadata(&file)?;
            let r = put_file(store, alg, &file, &meta)?;
            if let Some(n) = new.as_mut() {
                n.files.insert(path.as_str().to_string(), (Stat::of(&meta), r.clone()));
            }
            entries.insert(path, Node::File(r));
        }
    }
    let root = match (base, edits) {
        (Some((_, _, r)), Some(mut edits)) => {
            for (p, n) in entries {
                edits.insert(p, Edit::Set(n));
            }
            crate::update(store, alg, &r, &edits)?
        }
        _ => build(store, alg, format, &entries)?,
    };
    if let Some(n) = new.as_mut() {
        n.root = Some(root.clone());
        n.save(dir);
    }
    Ok(root)
}

fn put_file(store: &dyn Store, alg: &'static Algorithm, file: &FsPath, meta: &fs::Metadata) -> Result<View> {
    if meta.file_type().is_symlink() {
        put(store, alg, "symlink", &fs::read_link(file)?.into_os_string().into_vec())
    } else if meta.permissions().mode() & 0o111 != 0 {
        put(store, alg, "exec", &fs::read(file)?)
    } else {
        put(store, alg, "raw", &fs::read(file)?)
    }
}

struct Scan<'a> {
    store: &'a dyn Store,
    alg: &'static Algorithm,
    old: Option<&'a Index>,
    new: Option<&'a mut Index>,
    /// Files whose view came from the cache, to check against the store.
    reused: &'a mut Vec<(Path, std::path::PathBuf)>,
}

impl Scan<'_> {
    /// The edits that turn the last import (cached in `old`) into the
    /// directory, looking only at the paths a watcher logged since and what
    /// is under them: removals here, and what is there now in `entries` (to
    /// be set). The cache entries of everything else carry over.
    fn watched(
        &mut self,
        dir: &FsPath,
        old: &Index,
        changed: &BTreeSet<String>,
        entries: &mut BTreeMap<Path, Node>,
    ) -> Result<BTreeMap<Path, Edit>> {
        use crate::watch::touched;
        if let Some(n) = self.new.as_deref_mut() {
            n.dirs.extend(old.dirs.iter().filter(|d| !touched(d, changed)).cloned());
            n.files.extend(old.files.iter().filter(|(p, _)| !touched(p, changed)).map(|(p, e)| (p.clone(), e.clone())));
        }
        let mut edits = BTreeMap::new();
        // Each changed path once: one under another changed path is looked
        // at with it.
        for p in changed {
            let above = p.rfind('/').map(|i| &p[..i]);
            if above.is_some_and(|a| touched(a, changed)) || !p.contains('/') && crate::watch::own_name(p) {
                continue;
            }
            let path = Path::new(p.clone())?;
            let file = dir.join(p);
            let parent_ok = above.is_none_or(|a| fs::symlink_metadata(dir.join(a)).is_ok_and(|m| m.is_dir()));
            match fs::symlink_metadata(&file) {
                Ok(meta) if parent_ok => self.one(&file, path.clone(), &meta, entries)?,
                _ => {}
            }
            // What the last import had at or under the path and is gone.
            let under = format!("{p}/");
            let was = old
                .files
                .range(p.clone()..)
                .map(|(q, _)| q)
                .take_while(|q| *q == p || q.starts_with(&under))
                .chain(old.dirs.range(p.clone()..).take_while(|q| *q == p || q.starts_with(&under)));
            for q in was {
                let q = Path::new(q.clone())?;
                if !entries.contains_key(&q) {
                    edits.insert(q, Edit::Remove);
                }
            }
        }
        Ok(edits)
    }

    fn dir(&mut self, dir: &FsPath, at: Option<&Path>, entries: &mut BTreeMap<Path, Node>) -> Result<()> {
        for item in fs::read_dir(dir)? {
            let item = item?;
            let name = item.file_name().into_string().map_err(|n| Error::BadPath(n.to_string_lossy().into_owned()))?;
            if at.is_none() && crate::watch::own_name(&name) {
                continue;
            }
            let path = Path::new(match at {
                Some(d) => format!("{d}/{name}"),
                None => name,
            })?;
            let meta = fs::symlink_metadata(item.path())?;
            self.one(&item.path(), path, &meta, entries)?;
        }
        Ok(())
    }

    /// The file or directory at `file` (with `meta`), as `path`.
    fn one(
        &mut self,
        file: &FsPath,
        path: Path,
        meta: &fs::Metadata,
        entries: &mut BTreeMap<Path, Node>,
    ) -> Result<()> {
        {
            if meta.is_dir() {
                self.dir(file, Some(&path), entries)?;
                if let Some(n) = self.new.as_deref_mut() {
                    n.dirs.insert(path.as_str().to_string());
                }
                entries.insert(path, Node::Dir);
                return Ok(());
            }
            let stat = Stat::of(meta);
            let r = match self.old.and_then(|o| o.fresh(path.as_str(), &stat)) {
                Some(v) => {
                    self.reused.push((path.clone(), file.to_path_buf()));
                    v.clone()
                }
                None => put_file(self.store, self.alg, file, meta)?,
            };
            if let Some(n) = self.new.as_deref_mut() {
                n.files.insert(path.as_str().to_string(), (stat, r.clone()));
            }
            entries.insert(path, Node::File(r));
        }
        Ok(())
    }
}

/// Write what is at or under `prefix` (all for `None`) into `dest`, which
/// should be empty. Files must be in `raw`, `exec`, `symlink` or
/// `git-blob`. Returns how many paths were written.
pub fn checkout(store: &dyn Store, root: &View, prefix: Option<&Path>, dest: &FsPath) -> Result<usize> {
    checkout_indexed(store, root, prefix, dest, None)
}

/// [`checkout`], recording each written file in a stat cache for the store
/// named `scope`, so a commit of `dest` right after reads nothing again.
/// Only a whole checkout (no prefix) writes the cache.
pub fn checkout_indexed(
    store: &dyn Store,
    root: &View,
    prefix: Option<&Path>,
    dest: &FsPath,
    scope: Option<&str>,
) -> Result<usize> {
    let mut cache = scope.filter(|_| prefix.is_none()).map(Index::new);
    let n = write_walk(store, root, prefix, dest, cache.as_mut())?;
    if let Some(c) = cache.as_mut() {
        c.save(dest);
    }
    Ok(n)
}

/// Write what `root` holds at and under `prefix` into `dest`, recording
/// written files in `cache`.
fn write_walk(
    store: &dyn Store,
    root: &View,
    prefix: Option<&Path>,
    dest: &FsPath,
    mut cache: Option<&mut Index>,
) -> Result<usize> {
    let mut n = 0;
    for item in walk(store, root, prefix) {
        let (path, node) = item?;
        let target = dest.join(path.as_str());
        let r = match node {
            Node::Dir => {
                fs::create_dir_all(&target)?;
                n += 1;
                continue;
            }
            Node::File(r) => r,
        };
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let format = write_file(store, &r, &target)?;
        // A git blob is stored with its header, so a commit would hash the
        // file to another view: record only what a commit would reproduce.
        if let Some(c) = cache.as_deref_mut()
            && format != "git-blob"
        {
            c.files.insert(path.as_str().to_string(), (Stat::of(&fs::symlink_metadata(&target)?), r.clone()));
        }
        n += 1;
    }
    Ok(n)
}

/// The sparse-checkout file at the top of a working directory: the paths
/// checked out, one per line. A commit of the directory keeps everything
/// else as the base version has it. Like the stat cache it is never
/// imported, but unlike it, it is not a cache: deleting it makes the
/// directory a whole checkout, and a commit then drops what is missing.
pub const SPARSE: &str = ".cas-sparse";

/// The paths of `dir`'s sparse checkout, or `None` for a whole checkout.
pub fn sparse_paths(dir: &FsPath) -> Result<Option<Vec<Path>>> {
    match fs::read_to_string(dir.join(SPARSE)) {
        Ok(t) => Ok(Some(t.lines().filter(|l| !l.is_empty()).map(Path::new).collect::<Result<_>>()?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Check out only `paths` of `root` (each a file or a directory, with the
/// directories above it) into `dest` and record them in [`SPARSE`] (git's
/// sparse checkout, in cone mode). Paths `root` does not have are recorded
/// and written once they exist. Returns the number of paths written.
pub fn checkout_sparse(
    store: &dyn Store,
    root: &View,
    paths: &[Path],
    dest: &FsPath,
    scope: Option<&str>,
) -> Result<usize> {
    let mut cache = scope.map(Index::new);
    let mut n = 0;
    fs::create_dir_all(dest)?;
    for p in paths {
        n += write_walk(store, root, Some(p), dest, cache.as_mut())?;
    }
    let list: String = paths.iter().map(|p| format!("{p}\n")).collect();
    fs::write(dest.join(SPARSE), list)?;
    if let Some(c) = cache.as_mut() {
        c.save(dest);
    }
    Ok(n)
}

/// Import a working directory as a new version over `base`: the whole
/// directory ([`import_indexed`]), or for a sparse checkout, `base` with
/// the checked-out paths replaced by what the directory has there.
pub fn import_worktree(
    store: &dyn Store,
    alg: &'static Algorithm,
    format: &'static Format,
    dir: &FsPath,
    scope: Option<&str>,
    base: Option<&View>,
) -> Result<View> {
    let now = import_indexed(store, alg, format, dir, scope)?;
    let Some(paths) = sparse_paths(dir)? else { return Ok(now) };
    // Without a base, the paths outside the sparse ones would be lost.
    let base =
        base.ok_or_else(|| Error::Io(format!("{} is a sparse checkout and needs a base version", dir.display())))?;
    let gone: BTreeMap<Path, Edit> = paths.iter().map(|p| (p.clone(), Edit::Remove)).collect();
    let mut kept = crate::update(store, alg, base, &gone)?;
    let mut set = BTreeMap::new();
    for p in &paths {
        for item in walk(store, &now, Some(p)) {
            let (q, node) = item?;
            set.insert(q, Edit::Set(node));
        }
    }
    if !set.is_empty() {
        kept = crate::update(store, alg, &kept, &set)?;
    }
    Ok(kept)
}

/// Write the file `r` at `target` (which must not exist), returning its
/// format's alias.
fn write_file(store: &dyn Store, r: &View, target: &FsPath) -> Result<&'static str> {
    let format = r.known_format().ok_or_else(|| Error::UnknownFormat(r.format().to_string()))?;
    let bytes = read_blob(store, r.key())?;
    format.check(r.key(), &bytes)?;
    match format.alias {
        "symlink" => symlink(OsString::from_vec(bytes), target)?,
        "raw" | "exec" | "git-blob" => {
            let contents = if format.alias == "git-blob" { git_blob_contents(&bytes).unwrap() } else { &bytes };
            fs::write(target, contents)?;
            let mode = if format.alias == "exec" { 0o755 } else { 0o644 };
            fs::set_permissions(target, fs::Permissions::from_mode(mode))?;
        }
        other => return Err(Error::UnknownFormat(format!("cannot check out {other}"))),
    }
    Ok(format.alias)
}

/// A store that reads through to `base` and keeps its own writes in
/// memory: what `status` hashes a directory into, so the store is not
/// written.
struct Overlay<'a> {
    base: &'a dyn Store,
    top: cas_core::Session,
}

impl<'a> Overlay<'a> {
    fn new(base: &'a dyn Store) -> Overlay<'a> {
        let core = cas_core::CoreStore::new(
            cas_core::mem::MemBackend::new(),
            cas_core::AllowAll,
            cas_core::Policy::permissive(),
        );
        Overlay { base, top: core.session(cas_core::Who::anonymous()) }
    }
}

impl Store for Overlay<'_> {
    fn read(&self, key: &str) -> cas_core::Result<Option<Vec<u8>>> {
        match self.top.read(key)? {
            Some(v) => Ok(Some(v)),
            None => self.base.read(key),
        }
    }
    fn write(&self, key: &str, value: &[u8], expect: &cas_core::Expect) -> cas_core::Result<()> {
        self.top.write(key, value, expect)
    }
    fn remove(&self, _: &str, _: &cas_core::Expect) -> cas_core::Result<()> {
        Err(cas_core::Error::Denied)
    }
    fn list(&self, span: &cas_core::Span) -> cas_core::Result<cas_core::ListPage> {
        self.base.list(span)
    }
    fn has_many(&self, keys: &[String]) -> cas_core::Result<Vec<bool>> {
        let (a, b) = (self.top.has_many(keys)?, self.base.has_many(keys)?);
        Ok(a.into_iter().zip(b).map(|(x, y)| x || y).collect())
    }
}

/// How `dir` differs from `version`: `old` is the version's side, `new`
/// the directory's (git's `status`, with no staging area). Nothing is
/// written to the store; the stat cache spares hashing unchanged files.
pub fn status(
    store: &dyn Store,
    alg: &'static Algorithm,
    format: &'static Format,
    dir: &FsPath,
    scope: Option<&str>,
    version: &View,
) -> Result<Vec<crate::Change>> {
    let o = Overlay::new(store);
    let now = import_worktree(&o, alg, format, dir, scope, Some(version))?;
    crate::diff(&o, version, &now, None)
}

/// Make `dir` hold `version` again, as a fresh checkout would (git's
/// `reset --hard`): changed files are rewritten, extra paths removed,
/// missing ones written. With `keep_changes`, only paths the version does
/// not have are removed and the rest left alone (git's `clean`). Returns
/// the paths changed on disk.
pub fn reset(
    store: &dyn Store,
    alg: &'static Algorithm,
    format: &'static Format,
    dir: &FsPath,
    scope: Option<&str>,
    version: &View,
    keep_changes: bool,
) -> Result<Vec<Path>> {
    let mut done = Vec::new();
    // Directories removed from disk: the changes under them are done.
    let mut removed: Vec<Path> = Vec::new();
    for c in status(store, alg, format, dir, scope, version)? {
        if removed.iter().any(|r| c.path.starts_with(r)) {
            continue;
        }
        if keep_changes && c.old.is_some() {
            continue;
        }
        let target = dir.join(c.path.as_str());
        if c.new.is_some() {
            match fs::symlink_metadata(&target) {
                Ok(m) if m.is_dir() => {
                    // Everything under it is gone from disk too.
                    fs::remove_dir_all(&target)?;
                    removed.push(c.path.clone());
                }
                Ok(_) => fs::remove_file(&target)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        match &c.old {
            None => {}
            Some(Node::Dir) => fs::create_dir_all(&target)?,
            Some(Node::File(r)) => {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                write_file(store, r, &target)?;
            }
        }
        done.push(c.path);
    }
    // Refresh the stat cache for what was rewritten.
    if scope.is_some() {
        import_indexed(&Overlay::new(store), alg, format, dir, scope)?;
    }
    Ok(done)
}
