//! Pack files for the disk backend (spec/performance.md, step 2).
//!
//! A batch of blob files (and chunk files) is written as one pack instead of
//! one file each: one file to create, one to sync. Names are blob keys, or
//! `#<hex>` for chunks. A frame holds a name's file bytes in the disk
//! encoding (`0`/`1`/`2` header), or a delta: the value compressed by zstd
//! with another name's value as its dictionary. Deltas are made only by
//! [`Packs::repack`], from hints the writer supplies (the same path in the
//! predecessor), and decode back to a plain `0`-header file, so nothing above
//! this module sees them.
//!
//! Layout under `packs/`:
//! - `<id>.pack`: `CASPACK1`, then frames `kind u8, name len u16, name,
//!   data len u32, data`. Kind 0 is file bytes; kind 1 is a delta, whose data
//!   is `base name len u16, base name, value len u32, zstd bytes`.
//! - `<id>.idx`: `CASIDX01`, then `name len u16, name, offset u64` for each
//!   frame. A pack counts only once its index exists; both are synced before
//!   the index is renamed into place.
//! - `removed`: names removed from packs, one per line, replaced atomically.
//!
//! Every process sharing the root sees new packs and removals: a lookup
//! re-reads the indexes when the directory's modification time changed.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::SystemTime;

const PACK_MAGIC: &[u8; 8] = b"CASPACK1";
const IDX_MAGIC: &[u8; 8] = b"CASIDX01";
const FILE: u8 = 0;
const DELTA: u8 = 1;
/// Longest chain of deltas `repack` builds.
pub const MAX_DEPTH: usize = 10;

static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[derive(Clone, Copy)]
struct Loc {
    pack: usize,
    offset: u64,
}

#[derive(Default)]
struct State {
    /// Pack ids, in load order; `Loc::pack` indexes this.
    packs: Vec<String>,
    names: BTreeMap<String, Loc>,
    removed: BTreeSet<String>,
    seen_mtime: Option<SystemTime>,
}

pub struct Packs {
    dir: PathBuf,
    state: RwLock<State>,
}

fn bad(what: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("pack: {what}"))
}

fn take<'a>(b: &mut &'a [u8], n: usize) -> io::Result<&'a [u8]> {
    let (head, tail) = b.split_at_checked(n).ok_or_else(|| bad("truncated"))?;
    *b = tail;
    Ok(head)
}

fn take_u16(b: &mut &[u8]) -> io::Result<usize> {
    Ok(u16::from_be_bytes(take(b, 2)?.try_into().unwrap()) as usize)
}

fn take_u32(b: &mut &[u8]) -> io::Result<usize> {
    Ok(u32::from_be_bytes(take(b, 4)?.try_into().unwrap()) as usize)
}

fn put_name(out: &mut Vec<u8>, name: &str) {
    out.extend_from_slice(&(name.len() as u16).to_be_bytes());
    out.extend_from_slice(name.as_bytes());
}

/// A frame to write: plain file bytes, or a value of `len` bytes compressed
/// with the value of `base` as zstd dictionary.
pub enum Entry {
    File(Vec<u8>),
    Delta { base: String, len: usize, z: Vec<u8> },
}

impl Entry {
    /// A delta of `value` against `base_value`, if that is smaller than `file`.
    pub fn delta(base: &str, base_value: &[u8], value: &[u8], file: &[u8]) -> io::Result<Option<Entry>> {
        let z = zstd::bulk::Compressor::with_dictionary(3, base_value)?.compress(value)?;
        Ok((z.len() + base.len() + 8 < file.len()).then(|| Entry::Delta {
            base: base.to_string(),
            len: value.len(),
            z,
        }))
    }
}

/// Writes one pack frame by frame; nothing is visible until [`PackWriter::finish`].
pub struct PackWriter<'a> {
    packs: &'a Packs,
    id: String,
    tmp: PathBuf,
    out: io::BufWriter<File>,
    offset: u64,
    idx: Vec<u8>,
}

impl PackWriter<'_> {
    pub fn add(&mut self, name: &str, e: &Entry) -> io::Result<()> {
        put_name(&mut self.idx, name);
        self.idx.extend_from_slice(&self.offset.to_be_bytes());
        let mut frame = Vec::new();
        let data_len;
        match e {
            Entry::File(bytes) => {
                frame.push(FILE);
                put_name(&mut frame, name);
                frame.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
                data_len = bytes.len();
                self.out.write_all(&frame)?;
                self.out.write_all(bytes)?;
            }
            Entry::Delta { base, len, z } => {
                let mut d = Vec::new();
                put_name(&mut d, base);
                d.extend_from_slice(&(*len as u32).to_be_bytes());
                d.extend_from_slice(z);
                frame.push(DELTA);
                put_name(&mut frame, name);
                frame.extend_from_slice(&(d.len() as u32).to_be_bytes());
                data_len = d.len();
                self.out.write_all(&frame)?;
                self.out.write_all(&d)?;
            }
        }
        self.offset += (frame.len() + data_len) as u64;
        Ok(())
    }

    /// Sync the pack and its index and make them visible.
    pub fn finish(self) -> io::Result<String> {
        let PackWriter { packs, id, tmp, out, idx, .. } = self;
        let f = out.into_inner().map_err(|e| e.into_error())?;
        f.sync_all()?;
        drop(f);
        fs::rename(tmp.join(format!("{id}.pack")), packs.dir.join(format!("{id}.pack")))?;
        let t = tmp.join(format!("{id}.idx"));
        let mut f = File::create(&t)?;
        f.write_all(&idx)?;
        f.sync_all()?;
        fs::rename(&t, packs.dir.join(format!("{id}.idx")))?;
        File::open(&packs.dir)?.sync_all()?;
        packs.refresh()?;
        Ok(id)
    }
}

impl Packs {
    pub fn open(dir: PathBuf) -> io::Result<Packs> {
        fs::create_dir_all(&dir)?;
        let p = Packs { dir, state: RwLock::new(State::default()) };
        p.refresh()?;
        Ok(p)
    }

    fn mtime(&self) -> Option<SystemTime> {
        fs::metadata(&self.dir).and_then(|m| m.modified()).ok()
    }

    /// Catch up with the directory if it changed since the last look: load
    /// new indexes, re-read removals, and start over if a pack went away.
    fn refresh(&self) -> io::Result<()> {
        let now = self.mtime();
        if now.is_some() && self.state.read().unwrap().seen_mtime == now {
            return Ok(());
        }
        let mut ids: Vec<String> = Vec::new();
        for e in fs::read_dir(&self.dir)? {
            let name = e?.file_name().to_string_lossy().into_owned();
            if let Some(id) = name.strip_suffix(".idx") {
                ids.push(id.to_string());
            }
        }
        ids.sort();
        let mut st = self.state.write().unwrap();
        let known: BTreeSet<&String> = st.packs.iter().collect();
        if !known.iter().all(|k| ids.binary_search(k).is_ok()) {
            *st = State::default();
        }
        let known: BTreeSet<String> = st.packs.iter().cloned().collect();
        for id in ids.into_iter().filter(|i| !known.contains(i)) {
            let pack = st.packs.len();
            let bytes = fs::read(self.dir.join(format!("{id}.idx")))?;
            let mut b = &bytes[..];
            if take(&mut b, 8)? != IDX_MAGIC {
                return Err(bad(format!("{id}.idx: not an index")));
            }
            while !b.is_empty() {
                let n = take_u16(&mut b)?;
                let name = std::str::from_utf8(take(&mut b, n)?).map_err(bad)?.to_string();
                let offset = u64::from_be_bytes(take(&mut b, 8)?.try_into().unwrap());
                st.names.insert(name, Loc { pack, offset });
            }
            st.packs.push(id);
        }
        st.removed = match fs::read_to_string(self.dir.join("removed")) {
            Ok(text) => text.lines().map(String::from).collect(),
            Err(_) => BTreeSet::new(),
        };
        st.seen_mtime = now;
        Ok(())
    }

    fn locate(&self, name: &str) -> io::Result<Option<(String, Loc)>> {
        // One stat per lookup keeps every process current: new packs and
        // removals both change the directory.
        self.refresh()?;
        let st = self.state.read().unwrap();
        if st.removed.contains(name) {
            return Ok(None);
        }
        Ok(st.names.get(name).map(|l| (st.packs[l.pack].clone(), *l)))
    }

    pub fn contains(&self, name: &str) -> io::Result<bool> {
        Ok(self.locate(name)?.is_some())
    }

    /// The file bytes stored for `name` (deltas resolved to a `0`-header file).
    pub fn file(&self, name: &str) -> io::Result<Option<Vec<u8>>> {
        self.file_at_depth(name, 0)
    }

    fn file_at_depth(&self, name: &str, depth: usize) -> io::Result<Option<Vec<u8>>> {
        if depth > MAX_DEPTH + 1 {
            return Err(bad(format!("delta chain too long at {name}")));
        }
        let Some((id, loc)) = self.locate(name)? else { return Ok(None) };
        let mut f = File::open(self.dir.join(format!("{id}.pack")))?;
        f.seek(SeekFrom::Start(loc.offset))?;
        let mut head = [0u8; 3];
        f.read_exact(&mut head)?;
        let mut skip = vec![0; u16::from_be_bytes([head[1], head[2]]) as usize];
        f.read_exact(&mut skip)?;
        let mut len = [0u8; 4];
        f.read_exact(&mut len)?;
        let mut data = vec![0; u32::from_be_bytes(len) as usize];
        f.read_exact(&mut data)?;
        match head[0] {
            FILE => Ok(Some(data)),
            DELTA => {
                let mut b = &data[..];
                let n = take_u16(&mut b)?;
                let base = std::str::from_utf8(take(&mut b, n)?).map_err(bad)?.to_string();
                let size = take_u32(&mut b)?;
                let base_file =
                    self.file_at_depth(&base, depth + 1)?.ok_or_else(|| bad(format!("base {base} gone")))?;
                let base_value = plain_value(&base_file)?;
                let value = zstd::bulk::Decompressor::with_dictionary(&base_value)?.decompress(b, size)?;
                Ok(Some([&[0u8][..], &value].concat()))
            }
            k => Err(bad(format!("frame kind {k}"))),
        }
    }

    /// Names held in packs, in `[from, to)` (`to` open when `None`).
    pub fn names_in(&self, from: &str, to: Option<&str>) -> io::Result<Vec<String>> {
        self.refresh()?;
        let st = self.state.read().unwrap();
        let range = st.names.range::<str, _>((
            std::ops::Bound::Included(from),
            to.map_or(std::ops::Bound::Unbounded, std::ops::Bound::Excluded),
        ));
        Ok(range.map(|(k, _)| k.clone()).filter(|k| !st.removed.contains(k)).collect())
    }

    /// Start a pack; temp files go in `tmp`.
    pub fn writer(&self, tmp: &Path) -> io::Result<PackWriter<'_>> {
        let id = format!(
            "{:020}-{}-{}",
            SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_nanos()),
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let mut out = io::BufWriter::new(File::create(tmp.join(format!("{id}.pack")))?);
        out.write_all(PACK_MAGIC)?;
        Ok(PackWriter {
            packs: self,
            id,
            tmp: tmp.to_path_buf(),
            out,
            offset: PACK_MAGIC.len() as u64,
            idx: IDX_MAGIC.to_vec(),
        })
    }

    /// Write `entries` as one pack, durably, and make it visible.
    pub fn write(&self, entries: &[(String, Entry)], tmp: &Path) -> io::Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let mut w = self.writer(tmp)?;
        for (name, e) in entries {
            w.add(name, e)?;
        }
        w.finish().map(|_| ())
    }

    /// Stop serving `names` from packs (their bytes go at the next repack).
    pub fn remove(&self, names: &[String], tmp: &Path) -> io::Result<()> {
        self.refresh()?;
        let mut removed = self.state.read().unwrap().removed.clone();
        let before = removed.len();
        for n in names {
            if self.state.read().unwrap().names.contains_key(n) {
                removed.insert(n.clone());
            }
        }
        if removed.len() == before {
            return Ok(());
        }
        let t = tmp.join(format!("removed-{}", std::process::id()));
        let mut f = File::create(&t)?;
        for n in &removed {
            writeln!(f, "{n}")?;
        }
        f.sync_all()?;
        fs::rename(&t, self.dir.join("removed"))?;
        File::open(&self.dir)?.sync_all()?;
        self.refresh()
    }

    /// Every name held in packs (not removed).
    pub fn all(&self) -> io::Result<Vec<String>> {
        self.names_in("", None)
    }

    /// The pack ids present now, for [`Packs::drop_packs`] after a repack.
    pub fn ids(&self) -> io::Result<Vec<String>> {
        self.refresh()?;
        Ok(self.state.read().unwrap().packs.clone())
    }

    /// Delete the packs `ids` (already copied into a new one) and forget
    /// removals, which no remaining pack holds.
    pub fn drop_packs(&self, ids: &[String]) -> io::Result<()> {
        for id in ids {
            // The index first: a pack without one is ignored.
            let _ = fs::remove_file(self.dir.join(format!("{id}.idx")));
            let _ = fs::remove_file(self.dir.join(format!("{id}.pack")));
        }
        let _ = fs::remove_file(self.dir.join("removed"));
        File::open(&self.dir)?.sync_all()?;
        self.refresh()
    }
}

/// The value inside file bytes of the disk encoding, for small (unchunked)
/// files: `0` raw, `1` zstd.
pub fn plain_value(file: &[u8]) -> io::Result<Vec<u8>> {
    match file.split_first() {
        Some((0, rest)) => Ok(rest.to_vec()),
        Some((1, rest)) => zstd::stream::decode_all(rest),
        _ => Err(bad("delta base is not a plain file")),
    }
}
