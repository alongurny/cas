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
    /// Pack files (encoded stores only): batches and repacks.
    packs: Option<crate::pack::Packs>,
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

/// One zstd frame: in one call when the frame states its size (ours do),
/// streamed otherwise.
pub(crate) fn unzstd(z: &[u8]) -> io::Result<Vec<u8>> {
    match zstd::zstd_safe::get_frame_content_size(z) {
        Ok(Some(n)) if n <= 1 << 32 => {
            thread_local! {
                static D: std::cell::RefCell<zstd::zstd_safe::DCtx<'static>> =
                    std::cell::RefCell::new(zstd::zstd_safe::DCtx::create());
            }
            let mut out = Vec::with_capacity(n as usize);
            D.with(|d| d.borrow_mut().decompress(&mut out, z))
                .map_err(|c| io::Error::other(format!("zstd: {}", zstd::zstd_safe::get_error_name(c))))?;
            Ok(out)
        }
        _ => zstd::stream::decode_all(z),
    }
}

fn unpack(file: &[u8]) -> io::Result<(u8, Vec<u8>)> {
    match file.split_first() {
        Some((&RAW, rest)) => Ok((RAW, rest.to_vec())),
        Some((&t @ (ZSTD | CHUNKED), rest)) => Ok((t, unzstd(rest)?)),
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
        let packs = if encoded { Some(crate::pack::Packs::open(root.join("packs"))?) } else { None };
        Ok(DiskBackend { root, lock: Mutex::new(lock), tmp_seq: AtomicU64::new(0), encoded, packs })
    }

    fn chunk_path(&self, id: &str) -> PathBuf {
        self.root.join("chunks").join(&id[..2]).join(id)
    }

    /// The bytes of a blob file in this store's encoding.
    fn encode(&self, value: &[u8], mut pending: Option<&mut Pending>) -> io::Result<Vec<u8>> {
        if !self.encoded {
            return Ok(value.to_vec());
        }
        if value.len() <= CHUNK_OVER {
            return pack(value);
        }
        let mut ids = Vec::new();
        for (id, piece) in crate::chunk::split(value) {
            self.store_chunk(&id, piece, pending.as_deref_mut())?;
            ids.extend_from_slice(&id);
        }
        let z = zstd::bulk::compress(&ids, 3)?;
        Ok([&[CHUNKED][..], &z].concat())
    }

    /// [`DiskBackend::encode`] without writing: the blob file, and the
    /// packed chunks it names that may need writing.
    fn encode_pure(&self, value: &[u8]) -> io::Result<(Vec<u8>, PackedChunks)> {
        if !self.encoded {
            return Ok((value.to_vec(), Vec::new()));
        }
        if value.len() <= CHUNK_OVER {
            return Ok((pack(value)?, Vec::new()));
        }
        let mut ids = Vec::new();
        let mut chunks = Vec::new();
        for (id, piece) in crate::chunk::split(value) {
            if !self.has_chunk(&hex::encode(id))? {
                chunks.push((id, pack(piece)?));
            }
            ids.extend_from_slice(&id);
        }
        let z = zstd::bulk::compress(&ids, 3)?;
        Ok(([&[CHUNKED][..], &z].concat(), chunks))
    }

    fn store_chunk(&self, id: &crate::chunk::Id, piece: &[u8], pending: Option<&mut Pending>) -> io::Result<()> {
        let path = self.chunk_path(&hex::encode(id));
        if self.has_chunk(&hex::encode(id))? { Ok(()) } else { self.put_file_with(&path, &pack(piece)?, pending) }
    }

    fn decode(&self, file: Vec<u8>) -> io::Result<Vec<u8>> {
        if !self.encoded {
            return Ok(file);
        }
        match unpack(&file)? {
            (CHUNKED, ids) => {
                let mut out = Vec::new();
                for id in ids.chunks(32) {
                    let piece = self
                        .chunk_file(&hex::encode(id))?
                        .ok_or_else(|| io::Error::other(format!("chunk {} is missing", hex::encode(id))))?;
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
            if let Some(ids) = self.chunk_ids_of(&k)? {
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
        if let Some(p) = &self.packs {
            let dead: Vec<String> =
                p.all()?.into_iter().filter(|n| n.strip_prefix('#').is_some_and(|h| !live.contains(h))).collect();
            removed += dead.len();
            p.remove(&dead, &self.root.join("tmp"))?;
        }
        Ok(removed)
    }

    /// Rewrite every blob and chunk into one pack, as deltas where `hints`
    /// names a base (the same path in the predecessor) and the delta is
    /// smaller. `order` comes first (oldest first, so bases precede what
    /// depends on them); everything else follows. Old packs and loose
    /// blob and chunk files are deleted once the new pack is durable. Not
    /// safe while another process writes to the store, like `sweep_chunks`.
    /// Whether the last repack placed `key` (it is in the sealed pack), so a
    /// repack needs no delta hint for it or for what it holds.
    pub fn sealed(&self, key: &str) -> io::Result<bool> {
        Ok(match &self.packs {
            Some(p) => p.sealed_frame(key)?.is_some(),
            None => false,
        })
    }

    pub fn repack(&self, order: &[String], hints: &std::collections::HashMap<String, String>) -> io::Result<Repacked> {
        use crate::pack::{Entry, plain_value};
        let Some(packs) = &self.packs else { return Err(io::Error::other("only encoded stores have packs")) };
        let old_packs = packs.ids()?;
        let mut loose_blobs = Vec::new();
        for k in self.all_keys()? {
            if matches!(classify(&k), Some(Parsed::Blob { .. })) && self.path(&k).exists() {
                loose_blobs.push(k);
            }
        }
        let mut loose_chunks = Vec::new();
        for fan in fs::read_dir(self.root.join("chunks"))? {
            for c in fs::read_dir(fan?.path())? {
                loose_chunks.push(format!("#{}", c?.file_name().to_string_lossy()));
            }
        }
        let mut names: Vec<String> = Vec::new();
        let mut placed = std::collections::HashSet::new();
        let everything: Vec<String> =
            packs.all()?.into_iter().chain(loose_blobs.iter().cloned()).chain(loose_chunks.iter().cloned()).collect();
        let present: std::collections::HashSet<&String> = everything.iter().collect();
        // A chunked blob brings its chunks just before it, and each chunk it
        // does not share with its hinted base is hinted to the base's chunk
        // at the same relative position: an edit's chunks delta against the
        // chunks they replaced.
        let mut hints = hints.clone();
        let chunk_ids = |key: &str| -> io::Result<Option<Vec<String>>> {
            Ok(self.chunk_ids_of(key)?.map(|ids| ids.chunks(32).map(|c| format!("#{}", hex::encode(c))).collect()))
        };
        for n in order.iter().chain(everything.iter()) {
            if !present.contains(n) || placed.contains(n) {
                continue;
            }
            if !n.starts_with('#')
                && let Some(ids) = chunk_ids(n)?
            {
                let base_ids = match hints.get(n) {
                    Some(b) => chunk_ids(b)?.unwrap_or_default(),
                    None => Vec::new(),
                };
                let shared: std::collections::HashSet<&String> = base_ids.iter().collect();
                for (i, c) in ids.iter().enumerate() {
                    if !shared.contains(c) && !base_ids.is_empty() {
                        let j = i * base_ids.len() / ids.len();
                        hints.entry(c.clone()).or_insert_with(|| base_ids[j].clone());
                    }
                    if present.contains(c) && placed.insert(c.clone()) {
                        names.push(c.clone());
                    }
                }
            }
            placed.insert(n.clone());
            names.push(n.clone());
        }
        let file_of = |n: &String| -> io::Result<Vec<u8>> {
            let f = match n.strip_prefix('#') {
                Some(h) => self.chunk_file(h)?,
                None => self.blob_file(n)?,
            };
            f.ok_or_else(|| io::Error::other(format!("{n} vanished during repack")))
        };
        let new_entry = |n: &String| -> io::Result<Tried> {
            let file = file_of(n)?;
            let delta = match hints.get(n).filter(|b| present.contains(*b)) {
                Some(base) => match (plain_value(&file), file_of(base).and_then(|b| plain_value(&b))) {
                    (Ok(v), Ok(bv)) => Entry::delta(base, &bv, &v, &file)?,
                    _ => None,
                },
                None => None,
            };
            Ok(Tried::New(file, delta))
        };
        let mut w = packs.writer(&self.root.join("tmp"))?;
        let mut depth: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut out = Repacked::default();
        let loose: std::collections::HashSet<&String> = loose_blobs.iter().chain(loose_chunks.iter()).collect();
        let mut sealed = std::collections::HashMap::new();
        for group in names.chunks(512) {
            // A frame of the sealed pack (the last repack's) is copied as it
            // is, and so is a packed delta whose base is already placed or a
            // packed whole frame with no base to try (git's delta reuse): a
            // gc recompresses only what came since. Anything else is read and
            // its hinted delta tried, on every core; frames are accepted in
            // order.
            let tried = parallel_map(group, |n| {
                if let Some(f) = packs.sealed_frame(n)? {
                    return Ok(Tried::Sealed(f));
                }
                if loose.contains(n) {
                    return new_entry(n);
                }
                match packs.frame(n)? {
                    Some(f) if f.base.is_some() || !hints.get(n).is_some_and(|b| present.contains(b)) => {
                        Ok(Tried::Reuse(f))
                    }
                    _ => new_entry(n),
                }
            })?;
            for (n, t) in group.iter().zip(tried) {
                let (file, delta) = match t {
                    Tried::Sealed(f) => match sealed_depth(packs, &present, &mut sealed, n)? {
                        Some(d) => {
                            if f.plain {
                                depth.insert(n.clone(), d);
                            }
                            w.add_frame(n, &f.bytes)?;
                            if f.base.is_some() {
                                out.deltas += 1
                            } else {
                                out.whole += 1
                            }
                            continue;
                        }
                        // Its base is gone or its chain too deep: store it whole.
                        None => (file_of(n)?, None),
                    },
                    Tried::Reuse(f) => {
                        let d = match &f.base {
                            None => Some(0),
                            Some(b) => base_depth(packs, &present, &mut sealed, &depth, b)?.map(|d| d + 1),
                        };
                        match d {
                            Some(d) => {
                                if f.plain {
                                    depth.insert(n.clone(), d);
                                }
                                w.add_frame(n, &f.bytes)?;
                                if f.base.is_some() {
                                    out.deltas += 1
                                } else {
                                    out.whole += 1
                                }
                                continue;
                            }
                            None => (file_of(n)?, None),
                        }
                    }
                    Tried::New(file, delta) => (file, delta),
                };
                let bd = match &delta {
                    Some(_) => base_depth(packs, &present, &mut sealed, &depth, &hints[n])?,
                    None => None,
                };
                match (delta, bd) {
                    (Some(d), Some(bd)) => {
                        w.add(n, &d)?;
                        depth.insert(n.clone(), bd + 1);
                        out.deltas += 1;
                    }
                    _ => {
                        if matches!(file.first(), Some(0 | 1)) {
                            depth.insert(n.clone(), 0);
                        }
                        w.add(n, &Entry::File(file))?;
                        out.whole += 1;
                    }
                }
            }
        }
        let id = w.finish()?;
        packs.drop_packs(&old_packs)?;
        packs.seal(&id, &self.root.join("tmp"))?;
        for k in &loose_blobs {
            let _ = fs::remove_file(self.path(k));
        }
        for c in &loose_chunks {
            let _ = fs::remove_file(self.chunk_path(&c[1..]));
        }
        syncfs_or_dirs(&self.root)?;
        Ok(out)
    }

    /// The chunk ids of a chunked blob, `None` for any other. A packed delta
    /// is never chunked, so it is not resolved.
    fn chunk_ids_of(&self, key: &str) -> io::Result<Option<Vec<u8>>> {
        let file = match read_opt(&self.path(key))? {
            Some(f) => f,
            None => match self.packs.as_ref().map(|p| p.frame(key)).transpose()?.flatten() {
                Some(f) if f.base.is_none() => f.bytes[f.data_at..].to_vec(),
                _ => return Ok(None),
            },
        };
        match file.first() {
            Some(&CHUNKED) => Ok(Some(unpack(&file)?.1)),
            _ => Ok(None),
        }
    }

    /// A blob's file bytes, loose or packed.
    fn blob_file(&self, key: &str) -> io::Result<Option<Vec<u8>>> {
        // Packs first: a lookup there costs no system call, and a store that
        // has been through gc holds nearly everything in them. A name in both
        // has the same bytes.
        if let Some(p) = &self.packs
            && let Some(f) = p.file(key)?
        {
            return Ok(Some(f));
        }
        read_opt(&self.path(key))
    }

    fn chunk_file(&self, hex: &str) -> io::Result<Option<Vec<u8>>> {
        // Packs first, as for blobs.
        if let Some(p) = &self.packs
            && let Some(f) = p.file(&format!("#{hex}"))?
        {
            return Ok(Some(f));
        }
        read_opt(&self.chunk_path(hex))
    }

    fn has_blob(&self, key: &str) -> io::Result<bool> {
        Ok(self.packs.as_ref().map_or(Ok(false), |p| p.contains(key))? || self.path(key).exists())
    }

    fn has_chunk(&self, hex: &str) -> io::Result<bool> {
        Ok(self.chunk_path(hex).exists()
            || self.packs.as_ref().map_or(Ok(false), |p| p.contains(&format!("#{hex}")))?)
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
        self.put_file_with(dest, value, None)
    }

    /// [`DiskBackend::put_file`], or with `pending` only the unsynced temp
    /// file, renamed later by [`DiskBackend::commit`].
    fn put_file_with(&self, dest: &Path, value: &[u8], pending: Option<&mut Pending>) -> io::Result<()> {
        let dir = dest.parent().expect("slot paths have a parent");
        fs::create_dir_all(dir)?;
        let tmp = self.root.join("tmp").join(format!(
            "{}-{}",
            std::process::id(),
            self.tmp_seq.fetch_add(1, Ordering::Relaxed)
        ));
        let mut f = File::create(&tmp)?;
        f.write_all(value)?;
        if let Some(p) = pending {
            p.push((tmp, dest.to_path_buf()));
            return Ok(());
        }
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, dest)?;
        sync_dir(dir)
    }

    /// Make a batch's temp files durable, rename them into place in order,
    /// and make the renames durable: two file-system syncs for the whole
    /// batch instead of two per file.
    fn commit(&self, pending: Pending) -> io::Result<()> {
        if pending.is_empty() {
            return Ok(());
        }
        self.sync_all_files(&pending)?;
        for (tmp, dest) in &pending {
            fs::rename(tmp, dest)?;
        }
        let dirs: std::collections::BTreeSet<&Path> = pending.iter().filter_map(|(_, d)| d.parent()).collect();
        self.sync_dirs(dirs)
    }

    #[cfg(target_os = "linux")]
    fn sync_all_files(&self, _: &Pending) -> io::Result<()> {
        syncfs(&self.root)
    }

    #[cfg(not(target_os = "linux"))]
    fn sync_all_files(&self, pending: &Pending) -> io::Result<()> {
        pending.iter().try_for_each(|(tmp, _)| File::open(tmp)?.sync_all())
    }

    #[cfg(target_os = "linux")]
    fn sync_dirs(&self, _: std::collections::BTreeSet<&Path>) -> io::Result<()> {
        syncfs(&self.root)
    }

    #[cfg(not(target_os = "linux"))]
    fn sync_dirs(&self, dirs: std::collections::BTreeSet<&Path>) -> io::Result<()> {
        dirs.into_iter().try_for_each(sync_dir)
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
        if let Some(p) = &self.packs {
            keys.extend(p.all()?.into_iter().filter(|n| !n.starts_with('#')));
            keys.sort_unstable();
            keys.dedup();
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

/// `f` over `items` on up to one thread per core, results in order.
fn parallel_map<T: Sync, U: Send>(items: &[T], f: impl Fn(&T) -> io::Result<U> + Sync) -> io::Result<Vec<U>> {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(items.len().max(1));
    if threads <= 1 {
        return items.iter().map(&f).collect();
    }
    let per = items.len().div_ceil(threads);
    std::thread::scope(|s| {
        let handles: Vec<_> =
            items.chunks(per).map(|part| s.spawn(|| part.iter().map(&f).collect::<io::Result<Vec<U>>>())).collect();
        let mut out = Vec::with_capacity(items.len());
        for h in handles {
            out.extend(h.join().expect("encoder panicked")?);
        }
        Ok(out)
    })
}

/// The chain depth of `n`'s frame in the sealed pack, if every base in the
/// chain is still present and it is within [`crate::pack::MAX_DEPTH`].
fn sealed_depth(
    packs: &crate::pack::Packs,
    present: &std::collections::HashSet<&String>,
    memo: &mut std::collections::HashMap<String, Option<usize>>,
    n: &str,
) -> io::Result<Option<usize>> {
    if let Some(d) = memo.get(n) {
        return Ok(*d);
    }
    let d = match packs.sealed_frame(n)? {
        None => None,
        Some(f) => match f.base {
            None => Some(0),
            Some(b) if present.contains(&b) => {
                sealed_depth(packs, present, memo, &b)?.map(|d| d + 1).filter(|d| *d <= crate::pack::MAX_DEPTH)
            }
            Some(_) => None,
        },
    };
    memo.insert(n.to_string(), d);
    Ok(d)
}

/// The depth of `base` as a delta base in the new pack: placed already, or
/// a sealed frame that will be copied; `None` when a delta on it would make
/// a chain too long or it cannot be a base.
fn base_depth(
    packs: &crate::pack::Packs,
    present: &std::collections::HashSet<&String>,
    memo: &mut std::collections::HashMap<String, Option<usize>>,
    placed: &std::collections::HashMap<String, usize>,
    base: &str,
) -> io::Result<Option<usize>> {
    let d = match placed.get(base) {
        Some(d) => Some(*d),
        None if present.contains(&base.to_string()) => match packs.sealed_frame(base)? {
            Some(f) if f.plain => sealed_depth(packs, present, memo, base)?,
            _ => None,
        },
        None => None,
    };
    Ok(d.filter(|d| *d < crate::pack::MAX_DEPTH))
}

/// A repack's look at one entry: a stored frame to copy, or file bytes and
/// the delta tried for them.
enum Tried {
    Sealed(crate::pack::Frame),
    Reuse(crate::pack::Frame),
    New(Vec<u8>, Option<crate::pack::Entry>),
}

/// What a repack wrote.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Repacked {
    pub deltas: usize,
    pub whole: usize,
}

#[cfg(target_os = "linux")]
fn syncfs_or_dirs(root: &Path) -> io::Result<()> {
    syncfs(root)
}

#[cfg(not(target_os = "linux"))]
fn syncfs_or_dirs(root: &Path) -> io::Result<()> {
    sync_dir(&root.join("blobs"))
}

/// Chunks to write: id and packed bytes.
type PackedChunks = Vec<(crate::chunk::Id, Vec<u8>)>;

/// Temp files written but not yet synced or renamed, and where each goes.
type Pending = Vec<(PathBuf, PathBuf)>;

/// Flush every file system buffer of the file system holding `path`.
#[cfg(target_os = "linux")]
fn syncfs(path: &Path) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let dir = File::open(path)?;
    // SAFETY: a valid open file descriptor.
    if unsafe { libc::syncfs(dir.as_raw_fd()) } == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
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
    fn has(&self, key: &str) -> io::Result<bool> {
        match classify(key) {
            Some(Parsed::Blob { .. }) => self.has_blob(key),
            _ => Ok(self.path(key).exists()),
        }
    }

    fn get(&self, key: &str) -> io::Result<Option<Vec<u8>>> {
        match classify(key) {
            Some(Parsed::Blob { .. }) => self.blob_file(key)?.map(|f| self.decode(f)).transpose(),
            _ => read_opt(&self.path(key)),
        }
    }

    fn put_blob(&self, key: &str, value: &[u8]) -> io::Result<()> {
        let dest = self.path(key);
        if self.has_blob(key)? {
            return Ok(());
        }
        // Renaming over a concurrent writer's copy is harmless: same bytes.
        // Chunks are written (and synced) before the file that names them.
        let file = self.encode(value, None)?;
        self.put_file(&dest, &file)
    }

    fn put_blobs(&self, blobs: &[(&str, &[u8])]) -> io::Result<()> {
        let mut todo: Vec<&(&str, &[u8])> = Vec::new();
        for b in blobs {
            if !self.has_blob(b.0)? {
                todo.push(b);
            }
        }
        // Compress on every core; then write in order, one sync for all.
        let encoded = parallel_map(&todo, |(_, v)| self.encode_pure(v))?;
        if let Some(packs) = &self.packs
            && todo.len() > 1
        {
            // One pack for the batch: chunks first, then the files naming them.
            let mut w = packs.writer(&self.root.join("tmp"))?;
            let mut chunks_seen = std::collections::HashSet::new();
            for (_, (_, chunks)) in todo.iter().zip(&encoded) {
                for (id, packed) in chunks {
                    let hex = hex::encode(id);
                    if chunks_seen.insert(hex.clone()) && !self.has_chunk(&hex)? {
                        w.add(&format!("#{hex}"), &crate::pack::Entry::File(packed.clone()))?;
                    }
                }
            }
            for ((key, _), (file, _)) in todo.iter().zip(encoded) {
                w.add(key, &crate::pack::Entry::File(file))?;
            }
            w.finish()?;
            packs.compact(&self.root.join("tmp"))?;
            return Ok(());
        }
        let mut pending = Vec::new();
        for ((key, _), (file, chunks)) in todo.iter().zip(encoded) {
            for (id, packed) in chunks {
                let path = self.chunk_path(&hex::encode(id));
                if !path.exists() {
                    self.put_file_with(&path, &packed, Some(&mut pending))?;
                }
            }
            self.put_file_with(&self.path(key), &file, Some(&mut pending))?;
        }
        self.commit(pending)
    }

    fn remove_blob(&self, key: &str) -> io::Result<()> {
        self.remove_file(&self.path(key))?;
        match &self.packs {
            Some(p) => p.remove(&[key.to_string()], &self.root.join("tmp")),
            None => Ok(()),
        }
    }

    fn chunked(&self) -> bool {
        self.encoded
    }

    fn get_chunk(&self, id: &crate::chunk::Id) -> io::Result<Option<Vec<u8>>> {
        match self.chunk_file(&hex::encode(id))? {
            Some(f) => Ok(Some(unpack(&f)?.1)),
            None => Ok(None),
        }
    }

    fn put_chunk(&self, id: &crate::chunk::Id, bytes: &[u8]) -> io::Result<()> {
        self.store_chunk(id, bytes, None)
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
        // Packed blobs in the span.
        let mut candidates = candidates;
        if let Some(p) = &self.packs {
            // Bounds that are not UTF-8 widen to open ones; the filter below is exact.
            let from = span.from.as_deref().and_then(|b| std::str::from_utf8(b).ok()).unwrap_or("");
            let to = span.to.as_deref().and_then(|b| std::str::from_utf8(b).ok());
            // Chunks (`#…`) sort before every key; start past them.
            let from = from.max("$");
            candidates.extend(p.names_in_limited(from, to, limit)?.into_iter().filter(|n| !n.starts_with('#')));
            candidates.sort_unstable();
            candidates.dedup();
        }
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
        match fs::read_dir(dir) {
            Ok(d) => d.map(|f| fs::read_dir(f.unwrap().path()).unwrap().count()).sum(),
            Err(_) => 0,
        }
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

    #[test]
    fn batches_go_to_packs_and_repack_makes_deltas() {
        let dir = tempfile::tempdir().unwrap();
        let b = DiskBackend::open(dir.path()).unwrap();
        let key = |v: &[u8]| crate::algo::lookup("blake3").unwrap().key(v);
        let base: Vec<u8> =
            (0..20_000u32).flat_map(|i| format!("line {i} of a file\n").into_bytes()).take(60_000).collect();
        let mut edited = base.clone();
        edited.splice(30_000..30_000, b"one edit ".iter().copied());
        let large = big(9, 200_000);
        let (k1, k2, k3) = (key(&base), key(&edited), key(&large));
        b.put_blobs(&[(&k1, &base), (&k2, &edited), (&k3, &large)]).unwrap();
        // One pack, no loose blob files.
        assert_eq!(files(&dir.path().join("blobs/blake3")), 0);
        let packs = |d: &Path| {
            fs::read_dir(d.join("packs"))
                .unwrap()
                .filter(|e| e.as_ref().unwrap().path().extension().is_some_and(|x| x == "pack" || x == "idx"))
                .count()
        };
        assert_eq!(packs(dir.path()), 2);
        // Another process sees it.
        let other = DiskBackend::open(dir.path()).unwrap();
        for (k, v) in [(&k1, &base), (&k2, &edited), (&k3, &large)] {
            assert_eq!(other.get(k).unwrap().as_ref(), Some(v));
        }
        assert_eq!(b.list(&Span::prefix("blake3:"), 10).unwrap().len(), 3);
        // A loose blob, then a repack with a hint: edited is a delta of base.
        let small = b"small".to_vec();
        let k4 = key(&small);
        b.put_blob(&k4, &small).unwrap();
        let hints = [(k2.clone(), k1.clone())].into_iter().collect();
        let r = b.repack(&[k1.clone(), k2.clone()], &hints).unwrap();
        assert_eq!(r.deltas, 1);
        assert_eq!(files(&dir.path().join("blobs/blake3")), 0);
        let size: u64 =
            fs::read_dir(dir.path().join("packs")).unwrap().map(|e| e.unwrap().metadata().unwrap().len()).sum();
        // The random large blob does not compress; the two text files together cost little.
        assert!(size < large.len() as u64 + 30_000, "{size} bytes");
        for (k, v) in [(&k1, &base), (&k2, &edited), (&k3, &large), (&k4, &small)] {
            assert_eq!(b.get(k).unwrap().as_ref(), Some(v));
            assert_eq!(DiskBackend::open(dir.path()).unwrap().get(k).unwrap().as_ref(), Some(v));
        }
        // Removing from a pack, and sweeping a removed big blob's chunks.
        b.remove_blob(&k3).unwrap();
        assert_eq!(b.get(&k3).unwrap(), None);
        assert!(!other.has_blob(&k3).unwrap());
        assert!(b.sweep_chunks().unwrap() > 0);
        assert_eq!(b.get(&k2).unwrap(), Some(edited.clone()));
        // A second repack copies the sealed pack's delta as it is.
        let r = b.repack(&[], &Default::default()).unwrap();
        assert_eq!(r.deltas, 1);
        assert!(b.sealed(&k2).unwrap());
        // Removing the base keeps the delta readable; the next repack writes
        // it whole, since its base is not kept.
        b.remove_blob(&k1).unwrap();
        assert_eq!(b.get(&k1).unwrap(), None);
        assert_eq!(b.get(&k2).unwrap().as_ref(), Some(&edited));
        let r = b.repack(&[], &Default::default()).unwrap();
        assert_eq!(r.deltas, 0);
        assert_eq!(b.get(&k2).unwrap().as_ref(), Some(&edited));
        assert_eq!(DiskBackend::open(dir.path()).unwrap().get(&k2).unwrap().as_ref(), Some(&edited));
        assert_eq!(b.get(&k1).unwrap(), None);
    }
}
