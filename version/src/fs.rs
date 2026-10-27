//! Moving versions to and from a local directory (Unix): import a directory
//! as a version, and check out a version, or only the part under a prefix.

use crate::{Entry, Error, Mode, Path, Result, Shape, read_blob, walk, write_blob};
use cas_core::Store;
use cas_core::algo::Algorithm;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path as FsPath;

/// Write every file under `dir` as a blob and build a version of them.
/// Empty directories are dropped; names must be valid UTF-8 path names.
pub fn import(store: &dyn Store, alg: &'static Algorithm, shape: &dyn Shape, dir: &FsPath) -> Result<String> {
    let mut entries = BTreeMap::new();
    scan(store, alg, dir, None, &mut entries)?;
    shape.build(store, alg, &entries)
}

fn scan(
    store: &dyn Store,
    alg: &'static Algorithm,
    dir: &FsPath,
    at: Option<&Path>,
    entries: &mut BTreeMap<Path, Entry>,
) -> Result<()> {
    for item in fs::read_dir(dir)? {
        let item = item?;
        let name = item.file_name().into_string().map_err(|n| Error::BadPath(n.to_string_lossy().into_owned()))?;
        let path = Path::new(match at {
            Some(d) => format!("{d}/{name}"),
            None => name,
        })?;
        let meta = fs::symlink_metadata(item.path())?;
        let (bytes, mode) = if meta.file_type().is_symlink() {
            (fs::read_link(item.path())?.into_os_string().into_encoded_bytes(), Mode::Link)
        } else if meta.is_dir() {
            scan(store, alg, &item.path(), Some(&path), entries)?;
            continue;
        } else if meta.permissions().mode() & 0o111 != 0 {
            (fs::read(item.path())?, Mode::Exec)
        } else {
            (fs::read(item.path())?, Mode::File)
        };
        let key = write_blob(store, alg, &bytes)?;
        entries.insert(path, Entry { key, mode });
    }
    Ok(())
}

/// Write the entries at or under `prefix` (all for `None`) into `dest`,
/// which should be empty. Returns how many entries were written.
pub fn checkout(store: &dyn Store, root: &str, prefix: Option<&Path>, dest: &FsPath) -> Result<usize> {
    let mut n = 0;
    for r in walk(store, root, prefix)? {
        let (path, entry) = r?;
        let target = dest.join(path.as_str());
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = read_blob(store, entry.key())?;
        match entry.mode {
            Mode::Link => {
                symlink(OsString::from_vec(bytes), &target)?;
            }
            Mode::File | Mode::Exec => {
                fs::write(&target, bytes)?;
                let mode = if entry.mode == Mode::Exec { 0o755 } else { 0o644 };
                fs::set_permissions(&target, fs::Permissions::from_mode(mode))?;
            }
        }
        n += 1;
    }
    Ok(n)
}
