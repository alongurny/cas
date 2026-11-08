//! On-disk backend.
//!
//! Layout under the root:
//! - `blobs/<alg>/<first two hex chars>/<digest hex>`
//! - `vars/<hex of the variable name's bytes>` (hex keeps bytewise order and any
//!   name safe as a file name)
//! - `tmp/` for writes in flight, renamed into place once synced
//! - `lock`, held exclusively around every variable CAS, so CAS is atomic across
//!   threads and processes sharing the root.

use crate::backend::Backend;
use crate::key::{Parsed, classify};
use crate::types::{Expect, Span};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

pub struct DiskBackend {
    root: PathBuf,
    lock: Mutex<File>,
    tmp_seq: AtomicU64,
}

impl DiskBackend {
    pub fn open(root: impl Into<PathBuf>) -> io::Result<DiskBackend> {
        let root = root.into();
        for dir in ["blobs", "vars", "tmp"] {
            fs::create_dir_all(root.join(dir))?;
        }
        let lock = OpenOptions::new().create(true).truncate(false).write(true).open(root.join("lock"))?;
        Ok(DiskBackend { root, lock: Mutex::new(lock), tmp_seq: AtomicU64::new(0) })
    }

    fn path(&self, key: &str) -> PathBuf {
        match classify(key) {
            Some(Parsed::Blob { alg, digest_hex }) => {
                self.root.join("blobs").join(alg).join(&digest_hex[..2.min(digest_hex.len())]).join(digest_hex)
            }
            _ => var_path(&self.root.join("vars"), key),
        }
    }

    /// Write `value` to `dest` atomically and durably: temp file, fsync,
    /// rename, fsync the directory.
    fn put_file(&self, dest: &Path, value: &[u8]) -> io::Result<()> {
        let dir = dest.parent().expect("slot paths have a parent");
        fs::create_dir_all(dir)?;
        let tmp = self.root.join("tmp").join(format!(
            "{}-{}",
            std::process::id(),
            self.tmp_seq.fetch_add(1, Ordering::Relaxed)
        ));
        let mut f = File::create(&tmp)?;
        f.write_all(value)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, dest)?;
        sync_dir(dir)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        match fs::remove_file(path) {
            Ok(()) => sync_dir(path.parent().expect("slot paths have a parent")),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// The one blob directory that holds every key `span` can contain, if
    /// the bounds share a prefix `<alg>:<two hex digits>`.
    fn fan_dir(&self, span: &Span) -> Option<(String, PathBuf)> {
        let (from, to) = (span.from.as_deref()?, span.to.as_deref()?);
        let common = from.iter().zip(to).take_while(|(a, b)| a == b).count();
        let prefix = std::str::from_utf8(&from[..common]).ok()?;
        let (alg, digest) = prefix.split_once(':')?;
        let fan = digest.get(..2)?;
        let ok = |s: &str, f: fn(u8) -> bool| !s.is_empty() && s.bytes().all(f);
        if !ok(alg, |b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || !ok(fan, |b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            return None;
        }
        Some((alg.to_string(), self.root.join("blobs").join(alg).join(fan)))
    }

    fn all_keys(&self) -> io::Result<Vec<String>> {
        let mut keys = Vec::new();
        var_names(&self.root.join("vars"), String::new(), &mut keys)?;
        for alg in fs::read_dir(self.root.join("blobs"))? {
            let alg = alg?;
            let alg_name = alg.file_name().to_string_lossy().into_owned();
            for fan in fs::read_dir(alg.path())? {
                for blob in fs::read_dir(fan?.path())? {
                    keys.push(format!("{alg_name}:{}", blob?.file_name().to_string_lossy()));
                }
            }
        }
        Ok(keys)
    }
}

/// File names are limited (often to 255 bytes), so a variable's hex name is cut
/// into pieces of at most `VAR_PIECE` characters. Every piece but the last
/// is a directory, marked with a trailing `-` so it never collides with a
/// file of the same hex. Variables up to 127 bytes are a single file, as before.
const VAR_PIECE: usize = 254;

fn var_path(vars: &Path, name: &str) -> PathBuf {
    let hex = hex::encode(name);
    let mut path = vars.to_path_buf();
    let mut rest = hex.as_str();
    while rest.len() > VAR_PIECE {
        path.push(format!("{}-", &rest[..VAR_PIECE]));
        rest = &rest[VAR_PIECE..];
    }
    path.push(rest);
    path
}

fn var_names(dir: &Path, prefix: String, keys: &mut Vec<String>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        match name.strip_suffix('-') {
            Some(piece) if entry.file_type()?.is_dir() => var_names(&entry.path(), prefix.clone() + piece, keys)?,
            _ => {
                let bytes = hex::decode(prefix.clone() + &name).map_err(io::Error::other)?;
                keys.push(String::from_utf8(bytes).map_err(io::Error::other)?);
            }
        }
    }
    Ok(())
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

fn read_opt(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(v) => Ok(Some(v)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

impl Backend for DiskBackend {
    fn get(&self, key: &str) -> io::Result<Option<Vec<u8>>> {
        read_opt(&self.path(key))
    }

    fn put_blob(&self, key: &str, value: &[u8]) -> io::Result<()> {
        let dest = self.path(key);
        if dest.exists() {
            return Ok(());
        }
        // Renaming over a concurrent writer's copy is harmless: same bytes.
        self.put_file(&dest, value)
    }

    fn remove_blob(&self, key: &str) -> io::Result<()> {
        self.remove_file(&self.path(key))
    }

    fn cas_var(&self, name: &str, expect: &Expect, new: Option<&[u8]>) -> io::Result<bool> {
        let lock = self.lock.lock().unwrap();
        lock.lock()?;
        let result = (|| {
            let path = self.path(name);
            if !expect.matches(read_opt(&path)?.as_deref()) {
                return Ok(false);
            }
            match new {
                Some(v) => self.put_file(&path, v)?,
                None => self.remove_file(&path)?,
            }
            Ok(true)
        })();
        lock.unlock()?;
        result
    }

    // Every key in a span starts with the common prefix of its bounds. When
    // that prefix fixes a blob algorithm and fan-out directory, as the
    // one-key spans of `has` do, only that directory is read; otherwise
    // every key is scanned.
    fn list(&self, span: &Span, limit: usize) -> io::Result<Vec<String>> {
        let candidates = match self.fan_dir(span) {
            Some((alg, dir)) => match fs::read_dir(&dir) {
                Ok(entries) => entries
                    .map(|e| e.map(|e| format!("{alg}:{}", e.file_name().to_string_lossy())))
                    .collect::<io::Result<Vec<_>>>()?,
                Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
                Err(e) => return Err(e),
            },
            None => self.all_keys()?,
        };
        let mut keys: Vec<String> = candidates.into_iter().filter(|k| span.contains(k.as_bytes())).collect();
        keys.sort_unstable();
        keys.truncate(limit);
        Ok(keys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_var_names() {
        let dir = tempfile::tempdir().unwrap();
        let b = DiskBackend::open(dir.path()).unwrap();
        let names = ["t".repeat(127), "u".repeat(128), "v".repeat(1000), format!("derived/{}/x", "a".repeat(200))];
        for n in &names {
            assert!(b.cas_var(n, &Expect::Absent, Some(n.as_bytes())).unwrap());
            assert_eq!(b.get(n).unwrap().as_deref(), Some(n.as_bytes()));
        }
        let mut want: Vec<String> = names.to_vec();
        want.sort();
        assert_eq!(b.list(&Span::all(), 100).unwrap(), want);
        assert!(b.cas_var(&names[2], &Expect::Value(names[2].clone().into_bytes()), None).unwrap());
        assert_eq!(b.get(&names[2]).unwrap(), None);
        assert_eq!(b.list(&Span::all(), 100).unwrap().len(), 3);
    }
}
