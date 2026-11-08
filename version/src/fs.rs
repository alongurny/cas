//! Moving versions to and from a local directory (Unix): import a directory
//! as a version, and check out a version, or only the part under a prefix.

use crate::formats::{Format, git_blob_contents};
use crate::index::{self, Index, Stat};
use crate::{Error, Node, Path, Result, View, build, put, read_blob, walk};
use cas_core::Store;
use cas_core::algo::Algorithm;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path as FsPath;

/// Write every file under `dir` as a blob (`raw`, `exec` or `symlink`) and
/// build a version of them, keeping empty directories. Directories are in
/// `format`. Names must be valid UTF-8 path names.
/// The stat cache file ([`index::FILE`]) at the top is never imported.
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
    let mut entries = BTreeMap::new();
    let mut reused = Vec::new();
    let mut scan = Scan { store, alg, old: old.as_ref(), new: new.as_mut(), reused: &mut reused };
    scan.dir(dir, None, &mut entries)?;
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
    let root = build(store, alg, format, &entries)?;
    if let Some(n) = new.as_mut() {
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
    fn dir(&mut self, dir: &FsPath, at: Option<&Path>, entries: &mut BTreeMap<Path, Node>) -> Result<()> {
        for item in fs::read_dir(dir)? {
            let item = item?;
            let name = item.file_name().into_string().map_err(|n| Error::BadPath(n.to_string_lossy().into_owned()))?;
            if at.is_none() && name == index::FILE {
                continue;
            }
            let path = Path::new(match at {
                Some(d) => format!("{d}/{name}"),
                None => name,
            })?;
            let meta = fs::symlink_metadata(item.path())?;
            if meta.is_dir() {
                self.dir(&item.path(), Some(&path), entries)?;
                entries.insert(path, Node::Dir);
                continue;
            }
            let stat = Stat::of(&meta);
            let r = match self.old.and_then(|o| o.fresh(path.as_str(), &stat)) {
                Some(v) => {
                    self.reused.push((path.clone(), item.path()));
                    v.clone()
                }
                None => put_file(self.store, self.alg, &item.path(), &meta)?,
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
        let format = r.known_format().ok_or_else(|| Error::UnknownFormat(r.format().to_string()))?;
        let bytes = read_blob(store, r.key())?;
        format.check(r.key(), &bytes)?;
        match format.alias {
            "symlink" => symlink(OsString::from_vec(bytes), &target)?,
            "raw" | "exec" | "git-blob" => {
                let contents = if format.alias == "git-blob" { git_blob_contents(&bytes).unwrap() } else { &bytes };
                fs::write(&target, contents)?;
                let mode = if format.alias == "exec" { 0o755 } else { 0o644 };
                fs::set_permissions(&target, fs::Permissions::from_mode(mode))?;
            }
            other => return Err(Error::UnknownFormat(format!("cannot check out {other}"))),
        }
        // A git blob is stored with its header, so a commit would hash the
        // file to another view: record only what a commit would reproduce.
        if let Some(c) = cache.as_mut()
            && format.alias != "git-blob"
        {
            c.files.insert(path.as_str().to_string(), (Stat::of(&fs::symlink_metadata(&target)?), r.clone()));
        }
        n += 1;
    }
    if let Some(c) = cache.as_mut() {
        c.save(dest);
    }
    Ok(n)
}
