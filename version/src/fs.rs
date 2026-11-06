//! Moving versions to and from a local directory (Unix): import a directory
//! as a version, and check out a version, or only the part under a prefix.

use crate::formats::{Format, git_blob_contents};
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
pub fn import(store: &dyn Store, alg: &'static Algorithm, format: &'static Format, dir: &FsPath) -> Result<View> {
    let mut entries = BTreeMap::new();
    scan(store, alg, dir, None, &mut entries)?;
    build(store, alg, format, &entries)
}

fn scan(
    store: &dyn Store,
    alg: &'static Algorithm,
    dir: &FsPath,
    at: Option<&Path>,
    entries: &mut BTreeMap<Path, Node>,
) -> Result<()> {
    for item in fs::read_dir(dir)? {
        let item = item?;
        let name = item.file_name().into_string().map_err(|n| Error::BadPath(n.to_string_lossy().into_owned()))?;
        let path = Path::new(match at {
            Some(d) => format!("{d}/{name}"),
            None => name,
        })?;
        let meta = fs::symlink_metadata(item.path())?;
        let r = if meta.file_type().is_symlink() {
            put(store, alg, "symlink", &fs::read_link(item.path())?.into_os_string().into_vec())?
        } else if meta.is_dir() {
            scan(store, alg, &item.path(), Some(&path), entries)?;
            entries.insert(path, Node::Dir);
            continue;
        } else if meta.permissions().mode() & 0o111 != 0 {
            put(store, alg, "exec", &fs::read(item.path())?)?
        } else {
            put(store, alg, "raw", &fs::read(item.path())?)?
        };
        entries.insert(path, Node::File(r));
    }
    Ok(())
}

/// Write what is at or under `prefix` (all for `None`) into `dest`, which
/// should be empty. Files must be in `raw`, `exec`, `symlink` or
/// `git-blob`. Returns how many paths were written.
pub fn checkout(store: &dyn Store, root: &View, prefix: Option<&Path>, dest: &FsPath) -> Result<usize> {
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
        n += 1;
    }
    Ok(n)
}
