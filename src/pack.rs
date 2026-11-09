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
//! - `<id>.idx`: `CASIDX02`, a count `n u32`, then `n` records `name start
//!   u32, name len u16, offset u64` sorted by name, then the names. A lookup
//!   is a binary search in the file's bytes, as in git's pack index, so
//!   opening a store does not read every name into a map. (`CASIDX01`, an
//!   unsorted list `name len u16, name, offset u64`, is still read.) A pack
//!   counts only once its index exists; both are synced before the index is
//!   renamed into place.
//! - `removed`: names removed from packs, one per line, replaced atomically.
//!   A removed name stays readable as a delta base until its pack is gone.
//! - `sealed`: the id of the pack the last repack wrote. Its frames were
//!   placed with their deltas, so the next repack copies them as they are
//!   and only places what came after.
//!
//! Every process sharing the root sees new packs and removals: a lookup
//! re-reads the indexes when the directory's modification time changed,
//! looking at it on every miss and at most every 50 ms on hits.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::SystemTime;

const PACK_MAGIC: &[u8; 8] = b"CASPACK1";
const IDX_MAGIC_V1: &[u8; 8] = b"CASIDX01";
const IDX_MAGIC: &[u8; 8] = b"CASIDX02";
const REC: usize = 14;
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

/// A pack file mapped read-only. Packs never change once renamed into
/// place, and a mapping outlives the file's removal, so reads need no
/// system call.
struct Map {
    ptr: *const u8,
    len: usize,
}

// The mapping is read-only and owned here.
unsafe impl Send for Map {}
unsafe impl Sync for Map {}

impl Map {
    fn open(path: &Path) -> io::Result<Map> {
        use std::os::fd::AsRawFd;
        let f = File::open(path)?;
        let len = f.metadata()?.len() as usize;
        if len == 0 {
            return Err(bad("empty pack"));
        }
        // SAFETY: a fresh read-only private mapping of a whole file we opened.
        let ptr =
            unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_PRIVATE, f.as_raw_fd(), 0) };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Map { ptr: ptr as *const u8, len })
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: the mapping lives as long as self and is never written.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl Drop for Map {
    fn drop(&mut self) {
        // SAFETY: unmapping what open mapped.
        unsafe { libc::munmap(self.ptr as *mut libc::c_void, self.len) };
    }
}

/// One pack's index, in its file's (version 2) bytes.
struct Idx {
    bytes: Vec<u8>,
    n: usize,
}

impl Idx {
    fn parse(id: &str, bytes: Vec<u8>) -> io::Result<Idx> {
        let magic = bytes.get(..8).ok_or_else(|| bad(format!("{id}.idx: truncated")))?;
        if magic == IDX_MAGIC_V1 {
            let mut b = &bytes[8..];
            let mut entries = Vec::new();
            while !b.is_empty() {
                let n = take_u16(&mut b)?;
                let name = std::str::from_utf8(take(&mut b, n)?).map_err(bad)?.to_string();
                entries.push((name, u64::from_be_bytes(take(&mut b, 8)?.try_into().unwrap())));
            }
            return Idx::parse(id, encode_idx(entries));
        }
        if magic != IDX_MAGIC {
            return Err(bad(format!("{id}.idx: not an index")));
        }
        let n = u32::from_be_bytes(bytes.get(8..12).ok_or_else(|| bad("idx truncated"))?.try_into().unwrap()) as usize;
        let idx = Idx { bytes, n };
        if idx.bytes.len() < 12 + n * REC {
            return Err(bad(format!("{id}.idx: truncated")));
        }
        Ok(idx)
    }

    /// Record `i`'s name bytes; a record pointing outside the file reads
    /// as empty (and so never matches).
    fn name_bytes(&self, i: usize) -> &[u8] {
        let r = &self.bytes[12 + i * REC..12 + (i + 1) * REC];
        let start = u32::from_be_bytes(r[..4].try_into().unwrap()) as usize;
        let len = u16::from_be_bytes(r[4..6].try_into().unwrap()) as usize;
        self.bytes.get(start..start + len).unwrap_or_default()
    }

    fn name(&self, i: usize) -> io::Result<&str> {
        std::str::from_utf8(self.name_bytes(i)).map_err(bad)
    }

    fn offset(&self, i: usize) -> u64 {
        let r = &self.bytes[12 + i * REC..12 + (i + 1) * REC];
        u64::from_be_bytes(r[6..].try_into().unwrap())
    }

    /// The first record whose name is not less than `name`.
    fn lower_bound(&self, name: &str) -> usize {
        let (mut lo, mut hi) = (0, self.n);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.name_bytes(mid) < name.as_bytes() {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    fn get(&self, name: &str) -> Option<u64> {
        let i = self.lower_bound(name);
        (i < self.n && self.name_bytes(i) == name.as_bytes()).then(|| self.offset(i))
    }
}

/// A version 2 index of `entries` (name, frame offset).
fn encode_idx(mut entries: Vec<(String, u64)>) -> Vec<u8> {
    entries.sort();
    entries.dedup_by(|a, b| a.0 == b.0);
    let mut out = IDX_MAGIC.to_vec();
    out.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    let mut at = 12 + entries.len() * REC;
    for (name, off) in &entries {
        out.extend_from_slice(&(at as u32).to_be_bytes());
        out.extend_from_slice(&(name.len() as u16).to_be_bytes());
        out.extend_from_slice(&off.to_be_bytes());
        at += name.len();
    }
    for (name, _) in &entries {
        out.extend_from_slice(name.as_bytes());
    }
    out
}

#[derive(Default)]
struct State {
    /// Pack ids, in load order; `Loc::pack` indexes this, `idx` and `maps`.
    packs: Vec<String>,
    idx: Vec<Idx>,
    maps: Vec<std::sync::OnceLock<std::sync::Arc<Map>>>,
    removed: BTreeSet<String>,
    /// The index in `packs` of the sealed pack.
    sealed: Option<usize>,
    seen_mtime: Option<SystemTime>,
}

impl State {
    fn get(&self, name: &str) -> Option<Loc> {
        // Newest pack first.
        (0..self.packs.len()).rev().find_map(|pack| self.idx[pack].get(name).map(|offset| Loc { pack, offset }))
    }
}

pub struct Packs {
    dir: PathBuf,
    state: RwLock<State>,
    /// Values recently used as delta bases (git's delta base cache): a
    /// chain is resolved once, not once per object built on it.
    bases: std::sync::Mutex<BaseCache>,
    /// When a lookup last looked at the directory.
    looked: std::sync::Mutex<Option<std::time::Instant>>,
}

/// How long a lookup that finds its name trusts the last look at the
/// directory. A miss always looks again.
const FRESH: std::time::Duration = std::time::Duration::from_millis(50);

/// Bytes of base values kept.
const BASE_CACHE_BYTES: usize = 96 << 20;

#[derive(Default)]
struct BaseCache {
    map: std::collections::HashMap<String, (std::sync::Arc<Vec<u8>>, u64)>,
    bytes: usize,
    tick: u64,
}

impl BaseCache {
    fn get(&mut self, name: &str) -> Option<std::sync::Arc<Vec<u8>>> {
        self.tick += 1;
        let t = self.tick;
        self.map.get_mut(name).map(|(v, used)| {
            *used = t;
            v.clone()
        })
    }

    fn put(&mut self, name: &str, v: std::sync::Arc<Vec<u8>>) {
        if v.len() > BASE_CACHE_BYTES / 4 {
            return;
        }
        self.tick += 1;
        self.bytes += v.len();
        if let Some((old, _)) = self.map.insert(name.to_string(), (v, self.tick)) {
            self.bytes -= old.len();
        }
        if self.bytes > BASE_CACHE_BYTES {
            // Drop the older half: amortized, like a clock sweep.
            let mut ticks: Vec<u64> = self.map.values().map(|(_, t)| *t).collect();
            ticks.sort_unstable();
            let cut = ticks[ticks.len() / 2];
            self.map.retain(|_, (_, t)| *t > cut);
            self.bytes = self.map.values().map(|(v, _)| v.len()).sum();
        }
    }
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
        let z = with_dict_compress(base_value, value)?;
        Ok((z.len() + base.len() + 8 < file.len()).then(|| Entry::Delta {
            base: base.to_string(),
            len: value.len(),
            z,
        }))
    }
}

thread_local! {
    static DCTX: std::cell::RefCell<zstd::zstd_safe::DCtx<'static>> = std::cell::RefCell::new(zstd::zstd_safe::DCtx::create());
    static CCTX: std::cell::RefCell<zstd::zstd_safe::CCtx<'static>> = std::cell::RefCell::new(zstd::zstd_safe::CCtx::create());
}

fn zerr(code: usize) -> io::Error {
    io::Error::other(format!("zstd: {}", zstd::zstd_safe::get_error_name(code)))
}

/// `z` decompressed with `dict` as a raw-content dictionary, into exactly
/// `size` bytes. One context per thread: making a context and a digested
/// dictionary per delta cost more than the decompression.
fn with_dict_decompress(dict: &[u8], z: &[u8], size: usize) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(size);
    DCTX.with(|c| c.borrow_mut().decompress_using_dict(&mut out, z, dict)).map_err(zerr)?;
    if out.len() != size {
        return Err(bad("delta decodes to the wrong size"));
    }
    Ok(out)
}

fn with_dict_compress(dict: &[u8], value: &[u8]) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(zstd::zstd_safe::compress_bound(value.len()));
    CCTX.with(|c| c.borrow_mut().compress_using_dict(&mut out, value, dict, 3)).map_err(zerr)?;
    Ok(out)
}

/// A frame as stored: its bytes, the base it is a delta against, and
/// whether its value can serve as a delta base (an unchunked file).
pub struct Frame {
    pub bytes: Vec<u8>,
    /// Where the frame's data starts in `bytes`.
    pub data_at: usize,
    pub base: Option<String>,
    pub plain: bool,
}

/// Writes one pack frame by frame; nothing is visible until [`PackWriter::finish`].
pub struct PackWriter<'a> {
    packs: &'a Packs,
    id: String,
    tmp: PathBuf,
    out: io::BufWriter<File>,
    offset: u64,
    idx: Vec<(String, u64)>,
}

impl PackWriter<'_> {
    /// Copy a whole frame (as stored in another pack) under `name`.
    pub fn add_frame(&mut self, name: &str, frame: &[u8]) -> io::Result<()> {
        self.idx.push((name.to_string(), self.offset));
        self.out.write_all(frame)?;
        self.offset += frame.len() as u64;
        Ok(())
    }

    pub fn add(&mut self, name: &str, e: &Entry) -> io::Result<()> {
        self.idx.push((name.to_string(), self.offset));
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
        f.write_all(&encode_idx(idx))?;
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
        let p =
            Packs { dir, state: RwLock::new(State::default()), bases: Default::default(), looked: Default::default() };
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
        // Forget packs that went away (merged or dropped), keeping the
        // indexes already read for the rest.
        if !st.packs.iter().all(|k| ids.binary_search(k).is_ok()) {
            let old = std::mem::take(&mut *st);
            for ((id, idx), map) in old.packs.into_iter().zip(old.idx).zip(old.maps) {
                if ids.binary_search(&id).is_ok() {
                    st.packs.push(id);
                    st.idx.push(idx);
                    st.maps.push(map);
                }
            }
        }
        let known: BTreeSet<String> = st.packs.iter().cloned().collect();
        for id in ids.into_iter().filter(|i| !known.contains(i)) {
            let idx = Idx::parse(&id, fs::read(self.dir.join(format!("{id}.idx")))?)?;
            st.idx.push(idx);
            st.maps.push(std::sync::OnceLock::new());
            st.packs.push(id);
        }
        st.removed = match fs::read_to_string(self.dir.join("removed")) {
            Ok(text) => text.lines().map(String::from).collect(),
            Err(_) => BTreeSet::new(),
        };
        st.sealed = fs::read_to_string(self.dir.join("sealed"))
            .ok()
            .and_then(|id| st.packs.iter().position(|p| *p == id.trim()));
        st.seen_mtime = now;
        Ok(())
    }

    /// Look at the directory unless a lookup did within [`FRESH`] (`force`
    /// looks anyway). New packs and removals both change the directory, so
    /// every process sharing the root sees them; a name found in the state
    /// last looked at is right (content-addressed), at worst removed since.
    fn look(&self, force: bool) -> io::Result<()> {
        let now = std::time::Instant::now();
        {
            let l = self.looked.lock().unwrap();
            if !force && l.is_some_and(|t| now.duration_since(t) < FRESH) {
                return Ok(());
            }
        }
        self.refresh()?;
        *self.looked.lock().unwrap() = Some(now);
        Ok(())
    }

    fn locate(&self, name: &str) -> io::Result<Option<(String, Loc)>> {
        for force in [false, true] {
            self.look(force)?;
            let st = self.state.read().unwrap();
            if st.removed.contains(name) {
                return Ok(None);
            }
            if let Some(l) = st.get(name) {
                return Ok(Some((st.packs[l.pack].clone(), l)));
            }
        }
        Ok(None)
    }

    /// Where `name` is, with the mapping of its pack (taken under the same
    /// look at the state, which a refresh may replace).
    /// A delta base (`with_removed`) is looked up in the state the lookup of
    /// its dependent refreshed.
    fn locate_mapped(&self, name: &str, with_removed: bool) -> io::Result<Option<(Loc, std::sync::Arc<Map>)>> {
        if with_removed {
            return self.mapped(self.state.read().unwrap(), |st| st.get(name));
        }
        for force in [false, true] {
            self.look(force)?;
            let st = self.state.read().unwrap();
            if st.removed.contains(name) {
                return Ok(None);
            }
            if st.get(name).is_some() {
                return self.mapped(st, |st| st.get(name));
            }
        }
        Ok(None)
    }

    /// The mapping of the pack `find` picks for `name`, looking again if a
    /// compaction merged it away.
    fn mapped(
        &self,
        st: std::sync::RwLockReadGuard<'_, State>,
        find: impl Fn(&State) -> Option<Loc>,
    ) -> io::Result<Option<(Loc, std::sync::Arc<Map>)>> {
        let Some(loc) = find(&st) else { return Ok(None) };
        let cell = &st.maps[loc.pack];
        if let Some(m) = cell.get() {
            return Ok(Some((loc, m.clone())));
        }
        match Map::open(&self.dir.join(format!("{}.pack", st.packs[loc.pack]))) {
            Ok(m) => Ok(Some((loc, cell.get_or_init(|| std::sync::Arc::new(m)).clone()))),
            // Merged away by a compaction since the last look: look again.
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                drop(st);
                self.state.write().unwrap().seen_mtime = None;
                self.refresh()?;
                let st = self.state.read().unwrap();
                let Some(loc) = find(&st) else { return Ok(None) };
                let m = Map::open(&self.dir.join(format!("{}.pack", st.packs[loc.pack])))?;
                Ok(Some((loc, st.maps[loc.pack].get_or_init(|| std::sync::Arc::new(m)).clone())))
            }
            Err(e) => Err(e),
        }
    }

    /// Whether `name` is held, as of a fresh look at the directory: a
    /// caller deciding whether to write it must see removals at once.
    pub fn contains(&self, name: &str) -> io::Result<bool> {
        self.look(true)?;
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
        // A base may have been removed: its dependents still need it.
        let Some((loc, map)) = self.locate_mapped(name, depth > 0)? else { return Ok(None) };
        // A value decoded lately as a base (a walk often reads a chain from
        // its newest end) is not decoded again.
        if depth == 0
            && let Some(v) = self.bases.lock().unwrap().get(name)
        {
            let mut f = Vec::with_capacity(v.len() + 1);
            f.push(0);
            f.extend_from_slice(&v);
            return Ok(Some(f));
        }
        let mut b = map.bytes().get(loc.offset as usize..).ok_or_else(|| bad("offset past the pack"))?;
        let kind = take(&mut b, 1)?[0];
        let n = take_u16(&mut b)?;
        take(&mut b, n)?;
        let n = take_u32(&mut b)?;
        let data = take(&mut b, n)?;
        match kind {
            FILE => Ok(Some(data.to_vec())),
            DELTA => {
                let mut b = data;
                let n = take_u16(&mut b)?;
                let base = std::str::from_utf8(take(&mut b, n)?).map_err(bad)?.to_string();
                let size = take_u32(&mut b)?;
                let base_value = self.base_value(&base, depth + 1)?;
                let value = with_dict_decompress(&base_value, b, size)?;
                let file = [&[0u8][..], &value].concat();
                // A walk reads the next version at the same path soon, a
                // delta on this one.
                if depth == 0 {
                    self.bases.lock().unwrap().put(name, std::sync::Arc::new(value));
                }
                Ok(Some(file))
            }
            k => Err(bad(format!("frame kind {k}"))),
        }
    }

    /// The frame stored for `name`, as it is in its pack, to copy into
    /// another pack unchanged (a repack reusing deltas, as git does).
    pub fn frame(&self, name: &str) -> io::Result<Option<Frame>> {
        let Some((loc, map)) = self.locate_mapped(name, false)? else { return Ok(None) };
        frame_at(&map, loc)
    }

    /// The frame of `name` in the sealed pack, if it is there and not removed.
    pub fn sealed_frame(&self, name: &str) -> io::Result<Option<Frame>> {
        self.look(false)?;
        let st = self.state.read().unwrap();
        if st.removed.contains(name) {
            return Ok(None);
        }
        let find = |st: &State| st.sealed.and_then(|p| st.idx[p].get(name).map(|offset| Loc { pack: p, offset }));
        let Some((loc, map)) = self.mapped(st, find)? else { return Ok(None) };
        frame_at(&map, loc)
    }

    /// Record `id` as the sealed pack.
    pub fn seal(&self, id: &str, tmp: &Path) -> io::Result<()> {
        let t = tmp.join(format!("sealed-{}", std::process::id()));
        let mut f = File::create(&t)?;
        f.write_all(id.as_bytes())?;
        f.sync_all()?;
        fs::rename(&t, self.dir.join("sealed"))?;
        File::open(&self.dir)?.sync_all()?;
        self.state.write().unwrap().seen_mtime = None;
        self.refresh()
    }
}

fn frame_at(map: &Map, loc: Loc) -> io::Result<Option<Frame>> {
    {
        let at = loc.offset as usize;
        let mut b = map.bytes().get(at..).ok_or_else(|| bad("offset past the pack"))?;
        let start = b.len();
        let kind = take(&mut b, 1)?[0];
        let n = take_u16(&mut b)?;
        take(&mut b, n)?;
        let n = take_u32(&mut b)?;
        let data = take(&mut b, n)?;
        let bytes = map.bytes()[at..at + start - b.len()].to_vec();
        let data_at = bytes.len() - data.len();
        match kind {
            FILE => Ok(Some(Frame { bytes, data_at, base: None, plain: matches!(data.first(), Some(0 | 1)) })),
            DELTA => {
                let mut d = data;
                let n = take_u16(&mut d)?;
                let base = std::str::from_utf8(take(&mut d, n)?).map_err(bad)?.to_string();
                Ok(Some(Frame { bytes, data_at, base: Some(base), plain: true }))
            }
            k => Err(bad(format!("frame kind {k}"))),
        }
    }
}

impl Packs {
    /// The plain value of `name`, a delta base, from the cache if there.
    fn base_value(&self, name: &str, depth: usize) -> io::Result<std::sync::Arc<Vec<u8>>> {
        if let Some(v) = self.bases.lock().unwrap().get(name) {
            return Ok(v);
        }
        let file = self.file_at_depth(name, depth)?.ok_or_else(|| bad(format!("base {name} gone")))?;
        let v = std::sync::Arc::new(plain_value(&file)?);
        self.bases.lock().unwrap().put(name, v.clone());
        Ok(v)
    }

    /// Names held in packs, in `[from, to)` (`to` open when `None`).
    pub fn names_in(&self, from: &str, to: Option<&str>) -> io::Result<Vec<String>> {
        self.names_in_limited(from, to, usize::MAX)
    }

    /// The first `limit` names held in packs in `[from, to)`: each pack
    /// gives at most `limit`, so a page costs its size, not the store's.
    pub fn names_in_limited(&self, from: &str, to: Option<&str>, limit: usize) -> io::Result<Vec<String>> {
        self.refresh()?;
        let st = self.state.read().unwrap();
        let mut out = BTreeSet::new();
        for idx in &st.idx {
            let mut taken = 0;
            for i in idx.lower_bound(from)..idx.n {
                let k = idx.name(i)?;
                if to.is_some_and(|t| k >= t) || taken >= limit {
                    break;
                }
                if !st.removed.contains(k) {
                    out.insert(k.to_string());
                    taken += 1;
                }
            }
        }
        Ok(out.into_iter().take(limit).collect())
    }

    /// Keep the number of packs logarithmic, like git's geometric repack
    /// (`git repack --geometric=2`): packs are in age order, and the newest
    /// ones are merged into one while the pack before them holds at most
    /// twice as many entries as they do together. Each entry is then
    /// copied O(log n) times in all, and a lookup searches O(log n) packs.
    /// Frames are copied as they are, deltas included. Best effort: another
    /// process compacting at the same time makes this one skip.
    pub fn compact(&self, tmp: &Path) -> io::Result<()> {
        use std::os::fd::AsRawFd;
        let lock = File::create(self.dir.join("compact.lock"))?;
        // SAFETY: flock on a descriptor we own; released when it closes.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Ok(());
        }
        self.refresh()?;
        let (ids, sizes): (Vec<String>, Vec<usize>) = {
            let st = self.state.read().unwrap();
            st.packs.iter().cloned().zip(st.idx.iter().map(|i| i.n)).unzip()
        };
        let Some(last) = ids.len().checked_sub(1) else { return Ok(()) };
        let (mut first, mut acc) = (last, sizes[last]);
        while first > 0 && sizes[first - 1] <= 2 * acc {
            first -= 1;
            acc += sizes[first];
        }
        if first == last {
            return Ok(());
        }
        let merge = &ids[first..];
        let mut w = self.writer(tmp)?;
        let mut seen = std::collections::HashSet::new();
        for id in merge {
            let idx = Idx::parse(id, fs::read(self.dir.join(format!("{id}.idx")))?)?;
            let map = Map::open(&self.dir.join(format!("{id}.pack")))?;
            for i in 0..idx.n {
                let name = idx.name(i)?;
                if !seen.insert(name.to_string()) {
                    continue;
                }
                let mut b = map.bytes().get(idx.offset(i) as usize..).ok_or_else(|| bad("offset past the pack"))?;
                let start = b.len();
                take(&mut b, 1)?;
                let n = take_u16(&mut b)?;
                take(&mut b, n)?;
                let n = take_u32(&mut b)?;
                take(&mut b, n)?;
                let len = start - b.len();
                let at = idx.offset(i) as usize;
                w.add_frame(name, &map.bytes()[at..at + len])?;
            }
        }
        w.finish()?;
        for id in merge {
            let _ = fs::remove_file(self.dir.join(format!("{id}.idx")));
            let _ = fs::remove_file(self.dir.join(format!("{id}.pack")));
        }
        File::open(&self.dir)?.sync_all()?;
        self.refresh()
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
            idx: Vec::new(),
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
            if self.state.read().unwrap().get(n).is_some() {
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
        Some((1, rest)) => crate::disk::unzstd(rest),
        _ => Err(bad("delta base is not a plain file")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_1_index_is_still_read() {
        let mut v1 = IDX_MAGIC_V1.to_vec();
        for (name, off) in [("zz", 30u64), ("a", 8), ("m", 20)] {
            put_name(&mut v1, name);
            v1.extend_from_slice(&off.to_be_bytes());
        }
        let idx = Idx::parse("x", v1).unwrap();
        assert_eq!((idx.get("a"), idx.get("m"), idx.get("zz"), idx.get("b")), (Some(8), Some(20), Some(30), None));
        assert_eq!(idx.name(idx.lower_bound("b")).unwrap(), "m");
    }

    #[test]
    fn compaction_keeps_the_pack_count_logarithmic_and_every_name() {
        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path().join("tmp");
        fs::create_dir_all(&tmp).unwrap();
        let p = Packs::open(dir.path().join("packs")).unwrap();
        let mut names = Vec::new();
        for i in 0..64 {
            let name = format!("blake3:{i:064x}");
            p.write(&[(name.clone(), Entry::File(format!("\0value {i}").into_bytes()))], &tmp).unwrap();
            p.compact(&tmp).unwrap();
            names.push(name);
        }
        let n = p.ids().unwrap().len();
        assert!(n <= 7, "{n} packs for 64 writes");
        let other = Packs::open(dir.path().join("packs")).unwrap();
        for (i, name) in names.iter().enumerate() {
            assert_eq!(other.file(name).unwrap(), Some(format!("\0value {i}").into_bytes()));
            assert_eq!(p.file(name).unwrap(), Some(format!("\0value {i}").into_bytes()));
        }
        assert_eq!(other.all().unwrap(), names);
    }

    #[test]
    fn a_damaged_index_is_refused_or_matches_nothing() {
        assert!(Idx::parse("x", b"CASIDX02\0\0\0\x05".to_vec()).is_err());
        let mut bytes = encode_idx(vec![("k".into(), 8)]);
        bytes[12..16].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(Idx::parse("x", bytes).unwrap().get("k"), None);
    }
}
