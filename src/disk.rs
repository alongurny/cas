//! On-disk backend.
//!
//! Layout under the root:
//! - `blobs/<alg>/<first two hex chars>/<digest hex>`
//! - `vars/<hex of the variable name's bytes>` (hex keeps bytewise order and any
//!   name safe as a file name)
//! - `chunks/<first two hex chars>/<blake3 hex of the chunk>`, pieces of large blobs
//! - `tmp/` for writes in flight, renamed into place once synced
//! - `format`, `2` for stores that encode blobs (below); a store without it
//!   keeps every blob file as its raw bytes, as stores did before
//!
//! Encoding (format 2) is invisible above the backend: a blob's key is still
//! the hash of its whole content. A blob file starts with one byte: `0` raw
//! bytes follow, `1` zstd-compressed bytes follow, `2` a zstd-compressed
//! list of 32-byte chunk ids follows. Blobs over [`CHUNK_OVER`] bytes are cut
//! at content-defined boundaries, so an edit inside a large blob (a history
//! that gained a node) changes only the chunks around it, and the rest are
//! shared on disk. Chunk files use the same `0`/`1` header.
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
    encoded: bool,
}

pub use crate::chunk::CHUNK_OVER;
const RAW: u8 = 0;
const ZSTD: u8 = 1;
const CHUNKED: u8 = 2;

/// `bytes` behind a one-byte header, compressed when that is smaller.
fn pack(bytes: &[u8]) -> io::Result<Vec<u8>> {
    let z = zstd::bulk::compress(bytes, 3)?;
    Ok(if z.len() < bytes.len() { [&[ZSTD][..], &z].concat() } else { [&[RAW][..], bytes].concat() })
}

fn unpack(file: &[u8]) -> io::Result<(u8, Vec<u8>)> {
    match file.split_first() {
        Some((&RAW, rest)) => Ok((RAW, rest.to_vec())),
        Some((&t @ (ZSTD | CHUNKED), rest)) => Ok((t, zstd::stream::decode_all(rest)?)),
        _ => Err(io::Error::other("unknown blob file encoding")),
    }
}

impl DiskBackend {
    pub fn open(root: impl Into<PathBuf>) -> io::Result<DiskBackend> {
        let root = root.into();
        // A store with no blobs yet starts encoded; an older one stays raw.
        let marker = root.join("format");
        let fresh = !root.join("blobs").exists() || fs::read_dir(root.join("blobs"))?.next().is_none();
        for dir in ["blobs", "vars", "tmp", "chunks"] {
            fs::create_dir_all(root.join(dir))?;
        }
        let encoded = match fs::read_to_string(&marker) {
            Ok(v) => v.trim() == "2",
            Err(_) if fresh => {
                fs::write(&marker, "2\n")?;
                true
            }
            Err(_) => false,
        };
        let lock = OpenOptions::new().create(true).truncate(false).write(true).open(root.join("lock"))?;
        Ok(DiskBackend { root, lock: Mutex::new(lock), tmp_seq: AtomicU64::new(0), encoded })
    }

    fn chunk_path(&self, id: &str) -> PathBuf {
        self.root.join("chunks").join(&id[..2]).join(id)
    }

    /// The bytes of a blob file in this store's encoding.
    fn encode(&self, value: &[u8]) -> io::Result<Vec<u8>> {
        if !self.encoded {
            return Ok(value.to_vec());
        }
        if value.len() <= CHUNK_OVER {
            return pack(value);
        }
        let mut ids = Vec::new();
        for (id, piece) in crate::chunk::split(value) {
            self.store_chunk(&id, piece)?;
            ids.extend_from_slice(&id);
        }
        let z = zstd::bulk::compress(&ids, 3)?;
        Ok([&[CHUNKED][..], &z].concat())
    }

    fn store_chunk(&self, id: &crate::chunk::Id, piece: &[u8]) -> io::Result<()> {
        let path = self.chunk_path(&hex::encode(id));
        if path.exists() { Ok(()) } else { self.put_file(&path, &pack(piece)?) }
    }

    fn decode(&self, file: Vec<u8>) -> io::Result<Vec<u8>> {
        if !self.encoded {
            return Ok(file);
        }
        match unpack(&file)? {
            (CHUNKED, ids) => {
                let mut out = Vec::new();
                for id in ids.chunks(32) {
                    let path = self.chunk_path(&hex::encode(id));
                    let piece =
                        fs::read(&path).map_err(|e| io::Error::other(format!("chunk {}: {e}", hex::encode(id))))?;
                    out.extend(unpack(&piece)?.1);
                }
                Ok(out)
            }
            (_, bytes) => Ok(bytes),
        }
    }

    /// Remove chunks no blob file names any more. Removing a blob leaves its
    /// chunks, which other blobs may share; this sweeps them. Not safe to
    /// run while another process writes to the store.
    pub fn sweep_chunks(&self) -> io::Result<usize> {
        let mut live = std::collections::HashSet::new();
        for k in self.all_keys()? {
            if !matches!(classify(&k), Some(Parsed::Blob { .. })) {
                continue;
            }
            if let Some(file) = read_opt(&self.path(&k))?
                && let (CHUNKED, ids) = unpack(&file)?
            {
                live.extend(ids.chunks(32).map(hex::encode));
            }
        }
        let mut removed = 0;
        for fan in fs::read_dir(self.root.join("chunks"))? {
            for c in fs::read_dir(fan?.path())? {
                let c = c?;
                if !live.contains(&*c.file_name().to_string_lossy()) {
                    self.remove_file(&c.path())?;
                    removed += 1;
                }
            }
        }
        Ok(removed)
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
        let file = read_opt(&self.path(key))?;
        match (file, classify(key)) {
            (Some(f), Some(Parsed::Blob { .. })) => self.decode(f).map(Some),
            (f, _) => Ok(f),
        }
    }

    fn put_blob(&self, key: &str, value: &[u8]) -> io::Result<()> {
        let dest = self.path(key);
        if dest.exists() {
            return Ok(());
        }
        // Renaming over a concurrent writer's copy is harmless: same bytes.
        // Chunks are written (and synced) before the file that names them.
        let file = self.encode(value)?;
        self.put_file(&dest, &file)
    }

    fn remove_blob(&self, key: &str) -> io::Result<()> {
        self.remove_file(&self.path(key))
    }

    fn chunked(&self) -> bool {
        self.encoded
    }

    fn get_chunk(&self, id: &crate::chunk::Id) -> io::Result<Option<Vec<u8>>> {
        match read_opt(&self.chunk_path(&hex::encode(id)))? {
            Some(f) => Ok(Some(unpack(&f)?.1)),
            None => Ok(None),
        }
    }

    fn put_chunk(&self, id: &crate::chunk::Id, bytes: &[u8]) -> io::Result<()> {
        self.store_chunk(id, bytes)
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

    fn big(seed: u64, n: usize) -> Vec<u8> {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }

    fn files(dir: &std::path::Path) -> usize {
        fs::read_dir(dir).unwrap().map(|f| fs::read_dir(f.unwrap().path()).unwrap().count()).sum()
    }

    #[test]
    fn big_blobs_share_chunks_and_sweep() {
        let dir = tempfile::tempdir().unwrap();
        let b = DiskBackend::open(dir.path()).unwrap();
        assert!(b.chunked());
        let one = big(1, 300_000);
        let mut two = one.clone();
        two.splice(150_000..150_000, b"an edit in the middle".iter().copied());
        let (k1, k2) = ("blake3:".to_string() + &"1".repeat(64), "blake3:".to_string() + &"2".repeat(64));
        b.put_blob(&k1, &one).unwrap();
        let after_one = files(&dir.path().join("chunks"));
        b.put_blob(&k2, &two).unwrap();
        let added = files(&dir.path().join("chunks")) - after_one;
        assert!(added <= 3, "an edit adds a few chunks, not {added}");
        assert_eq!(b.get(&k1).unwrap().unwrap(), one);
        assert_eq!(b.get(&k2).unwrap().unwrap(), two);
        let id = crate::chunk::split(&one)[0].0;
        assert_eq!(b.get_chunk(&id).unwrap().unwrap(), crate::chunk::split(&one)[0].1);
        // Small blobs are compressed in place, not chunked.
        let k3 = "blake3:".to_string() + &"3".repeat(64);
        b.put_blob(&k3, &[7; 1000]).unwrap();
        assert!(b.cas_var("v", &Expect::Absent, Some(b"x")).unwrap());
        assert!(fs::metadata(b.path(&k3)).unwrap().len() < 100);
        assert_eq!(b.get(&k3).unwrap().unwrap(), vec![7; 1000]);
        b.remove_blob(&k2).unwrap();
        assert_eq!(b.sweep_chunks().unwrap(), added);
        assert_eq!(b.get(&k1).unwrap().unwrap(), one);
        b.remove_blob(&k1).unwrap();
        b.sweep_chunks().unwrap();
        assert_eq!(files(&dir.path().join("chunks")), 0);
    }

    #[test]
    fn an_old_raw_store_stays_raw() {
        let dir = tempfile::tempdir().unwrap();
        let k = "blake3:".to_string() + &"a".repeat(64);
        let path = dir.path().join("blobs/blake3/aa").join("a".repeat(64));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"plain").unwrap();
        let b = DiskBackend::open(dir.path()).unwrap();
        assert!(!b.chunked());
        assert_eq!(b.get(&k).unwrap().unwrap(), b"plain");
        let k2 = "blake3:".to_string() + &"b".repeat(64);
        b.put_blob(&k2, &big(2, 100_000)).unwrap();
        assert_eq!(fs::read(b.path(&k2)).unwrap(), big(2, 100_000));
        drop(b);
        assert!(!DiskBackend::open(dir.path()).unwrap().chunked());
    }
}
