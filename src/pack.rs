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
//! - `<id>.pack`: `CASPACK1`, then frames `kind u8, data len varint,
//!   data`, found through the index (which holds the names). Kind 4 is
//!   file bytes; kind 5 is a delta, whose data is `base, value len varint,
//!   zstd bytes`. The base is a binary name: `1` and the 32 digest bytes of
//!   a `blake3:` key, `2` and those of a chunk, or `0`, `len u16` and the
//!   name. Kind 6 is a delta on a frame earlier in the same pack, the base
//!   given as how far back that frame starts (a varint) instead of by
//!   name: a chain is followed without index lookups, as git's offset
//!   deltas are. Merges copy frames in pack order, so a base comes before
//!   its deltas and they stay kind 6. Older kinds still read: 2 and 3 are 4 and 5 with the frame's own
//!   binary name after the kind and `u32` lengths; 0 and 1 are 2 and 3 with
//!   every name as `len u16, name` (73 bytes for a blake3 key, not 33).
//! - `<id>.idx`: `CASIDX03`, counts `keys u32, chunks u32`, then that many
//!   records `digest [32], offset u64` sorted by digest, for `blake3:` keys
//!   and then for chunks, then a `CASIDX02` index of the other names: a
//!   count `n u32`, `n` records `name start u32, name len u16, offset u64`
//!   sorted by name, then the names. A lookup is a search in the file's
//!   bytes, as in git's pack index (interpolation, for digests), so opening
//!   a store does not read every name into a map. (`CASIDX02` files, and
//!   `CASIDX01`, an unsorted list `name len u16, name, offset u64`, are still
//!   read.) A pack
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
const IDX_MAGIC_V2: &[u8; 8] = b"CASIDX02";
const IDX_MAGIC: &[u8; 8] = b"CASIDX03";
const REC: usize = 14;
const FILE: u8 = 0;
const DELTA: u8 = 1;
/// As FILE and DELTA, with names (the frame's, a delta's base) in the
/// binary form of [`put_bin_name`]: what writers write now.
const FILE_B: u8 = 2;
const DELTA_B: u8 = 3;
/// As FILE_B and DELTA_B without the frame's own name (the index has it),
/// and with varint lengths: what writers write now.
const FILE_N: u8 = 4;
const DELTA_N: u8 = 5;
/// As DELTA_N with the base named by where its frame starts, as a varint
/// distance back from this frame's start (git's offset deltas): for a
/// base earlier in the same pack. No index lookup to follow a chain.
const DELTA_O: u8 = 6;
/// Longest chain of deltas a pack holds (git's default depth). At 10, a
/// file's every eleventh version was whole: git.git's store took 506 MB,
/// 350 MB at 50; reading the newest version of a file costs the most.
pub const MAX_DEPTH: usize = 50;

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
    /// Distinct for every mapping made by this process: the base cache's
    /// key, with a frame's offset.
    uid: u64,
}

static MAP_UID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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
        Ok(Map { ptr: ptr as *const u8, len, uid: MAP_UID.fetch_add(1, std::sync::atomic::Ordering::Relaxed) })
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

/// One pack's index, in its file's bytes: mapped, so opening a store reads
/// none of an index it does not search.
///
/// Version 3 holds two sections of fixed records, `digest [32], offset
/// u64` sorted by digest, for `blake3:` keys and for `#` chunk names,
/// then a version 2 index of every other name. Version 2 files are all
/// that last part.
struct Idx {
    bytes: IdxBytes,
    /// Records in the key and chunk sections.
    keys: usize,
    chunks: usize,
    /// Where the version 2 part starts, and its record count.
    base: usize,
    strs: usize,
    /// Lookups by binary search in the version 2 part so far; past
    /// [`HASH_AFTER`] a hash table of its records is built (a gc or clone
    /// of an old store looks up every key, in up to a dozen packs, and
    /// each search step is a cache miss).
    searches: std::sync::atomic::AtomicU32,
    table: std::sync::OnceLock<Vec<u32>>,
    /// Every record's (offset, number) by offset, built when an offset
    /// delta's base is first named (a repack, a copy).
    by_offset: std::sync::OnceLock<Vec<(u64, u32)>>,
    /// Lookups in the fixed sections so far; past [`HASH_AFTER`] each gets
    /// a fanout by its digests' leading bits (about four records a
    /// bucket), so a lookup costs a table read and a probe or two instead
    /// of an interpolation search's half dozen cache misses.
    fixed_searches: std::sync::atomic::AtomicU32,
    fans: [std::sync::OnceLock<Fan>; 2],
}

/// Bucket `b` of a fixed section holds records `starts[b]..starts[b + 1]`,
/// the digests whose leading `bits` bits are `b`.
struct Fan {
    bits: u32,
    starts: Vec<u32>,
}

impl Fan {
    fn build(r: &[u8]) -> Fan {
        let n = r.len() / FIXED;
        let bits = (n / 4).max(1).ilog2().clamp(1, 24);
        let mut starts = vec![0u32; (1 << bits) + 1];
        for i in 0..n {
            let b = (r[i * FIXED] as u32) << 16 | (r[i * FIXED + 1] as u32) << 8 | r[i * FIXED + 2] as u32;
            starts[(b >> (24 - bits)) as usize + 1] += 1;
        }
        for b in 1..starts.len() {
            starts[b] += starts[b - 1];
        }
        Fan { bits, starts }
    }

    fn range(&self, d: &[u8; 32]) -> (usize, usize) {
        let b = ((d[0] as u32) << 16 | (d[1] as u32) << 8 | d[2] as u32) >> (24 - self.bits);
        (self.starts[b as usize] as usize, self.starts[b as usize + 1] as usize)
    }
}

/// Binary searches in one index before it gets a hash table.
const HASH_AFTER: u32 = 4096;
/// A fixed record: 32 digest bytes and an offset.
const FIXED: usize = 40;

/// A name's slot hash: its last 8 bytes (a digest's hex is uniform), mixed.
fn name_hash(name: &[u8]) -> u64 {
    let mut t = [0u8; 8];
    let tail = &name[name.len().saturating_sub(8)..];
    t[..tail.len()].copy_from_slice(tail);
    let mut h = u64::from_le_bytes(t) ^ (name.len() as u64).rotate_left(56);
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51afd7ed558ccd);
    h ^ (h >> 33)
}

/// Lowercase hex digits' values; 0xff for any other byte.
const HEX: [u8; 256] = {
    let mut t = [0xffu8; 256];
    let mut i = 0;
    while i < 10 {
        t[b'0' as usize + i] = i as u8;
        i += 1;
    }
    let mut i = 0;
    while i < 6 {
        t[b'a' as usize + i] = 10 + i as u8;
        i += 1;
    }
    t
};

/// The 32 digest bytes of a canonical (lowercase) 64-digit hex string. A
/// table lookup per digit: parsing names was most of an index lookup.
pub(crate) fn hex32(h: &str) -> Option<[u8; 32]> {
    let b: &[u8; 64] = h.as_bytes().try_into().ok()?;
    let mut d = [0u8; 32];
    let mut bad = 0u8;
    for i in 0..32 {
        let (hi, lo) = (HEX[b[2 * i] as usize], HEX[b[2 * i + 1] as usize]);
        bad |= hi | lo;
        d[i] = hi << 4 | (lo & 0x0f);
    }
    (bad & 0xf0 == 0).then_some(d)
}

/// The fixed sections' name prefixes, in section order.
const SECTIONS: [&str; 2] = ["blake3:", "#"];

/// Which fixed section `name` belongs in, and its digest.
fn fixed_name(name: &str) -> Option<(usize, [u8; 32])> {
    SECTIONS.iter().enumerate().find_map(|(s, p)| name.strip_prefix(p).and_then(hex32).map(|d| (s, d)))
}

enum IdxBytes {
    Owned(Vec<u8>),
    Mapped(Map),
}

impl IdxBytes {
    fn get(&self) -> &[u8] {
        match self {
            IdxBytes::Owned(v) => v,
            IdxBytes::Mapped(m) => m.bytes(),
        }
    }
}

impl Idx {
    /// The index file at `path`, mapped when it is version 2 or 3.
    fn open(id: &str, path: &Path) -> io::Result<Idx> {
        let m = Map::open(path)?;
        if m.bytes().starts_with(IDX_MAGIC) || m.bytes().starts_with(IDX_MAGIC_V2) {
            return Idx::check(id, IdxBytes::Mapped(m));
        }
        Idx::parse(id, m.bytes().to_vec())
    }

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
        if magic != IDX_MAGIC && magic != IDX_MAGIC_V2 {
            return Err(bad(format!("{id}.idx: not an index")));
        }
        Idx::check(id, IdxBytes::Owned(bytes))
    }

    fn check(id: &str, bytes: IdxBytes) -> io::Result<Idx> {
        let b = bytes.get();
        let truncated = || bad(format!("{id}.idx: truncated"));
        let u32_at = |at: usize| -> io::Result<usize> {
            Ok(u32::from_be_bytes(b.get(at..at + 4).ok_or_else(truncated)?.try_into().unwrap()) as usize)
        };
        let (keys, chunks, base) = if b.starts_with(IDX_MAGIC) {
            let (k, c) = (u32_at(8)?, u32_at(12)?);
            (k, c, 16 + (k + c) * FIXED)
        } else {
            (0, 0, 0)
        };
        if b.get(base..base + 8) != Some(&IDX_MAGIC_V2[..]) {
            return Err(bad(format!("{id}.idx: not an index")));
        }
        let strs = u32_at(base + 8)?;
        if b.len() < base + 12 + strs * REC {
            return Err(truncated());
        }
        Ok(Idx {
            bytes,
            keys,
            chunks,
            base,
            strs,
            searches: Default::default(),
            table: Default::default(),
            by_offset: Default::default(),
            fixed_searches: Default::default(),
            fans: Default::default(),
        })
    }

    /// Every name in the index.
    fn len(&self) -> usize {
        self.keys + self.chunks + self.strs
    }

    /// Record number `i` (the key section, then chunks, then the version 2
    /// part): its name and offset.
    fn record(&self, i: usize) -> io::Result<(String, u64)> {
        let fixed = |s: usize, j: usize| {
            let rec = &self.fixed(s)[j * FIXED..(j + 1) * FIXED];
            (format!("{}{}", SECTIONS[s], hex::encode(&rec[..32])), u64::from_be_bytes(rec[32..].try_into().unwrap()))
        };
        if i < self.keys {
            Ok(fixed(0, i))
        } else if i < self.keys + self.chunks {
            Ok(fixed(1, i - self.keys))
        } else {
            let j = i - self.keys - self.chunks;
            Ok((self.name(j)?.to_string(), self.offset(j)))
        }
    }

    /// Every record's (offset, number), by offset.
    fn by_offset(&self) -> &[(u64, u32)] {
        self.by_offset.get_or_init(|| {
            let mut v: Vec<(u64, u32)> = (0..self.len()).map(|i| (self.offset_of(i), i as u32)).collect();
            v.sort_unstable();
            v
        })
    }

    /// Record `i`'s offset.
    fn offset_of(&self, i: usize) -> u64 {
        let fixed = |s: usize, j: usize| {
            let rec = &self.fixed(s)[j * FIXED..(j + 1) * FIXED];
            u64::from_be_bytes(rec[32..].try_into().unwrap())
        };
        if i < self.keys {
            fixed(0, i)
        } else if i < self.keys + self.chunks {
            fixed(1, i - self.keys)
        } else {
            self.offset(i - self.keys - self.chunks)
        }
    }

    /// The name of the frame starting at `offset`.
    fn name_at(&self, offset: u64) -> io::Result<String> {
        let by = self.by_offset();
        match by.binary_search_by_key(&offset, |&(o, _)| o) {
            Ok(i) => Ok(self.record(by[i].1 as usize)?.0),
            Err(_) => Err(bad(format!("no frame starts at {offset}"))),
        }
    }

    /// Fixed section `s`'s records.
    fn fixed(&self, s: usize) -> &[u8] {
        let start = 16 + if s == 0 { 0 } else { self.keys * FIXED };
        let n = if s == 0 { self.keys } else { self.chunks };
        &self.bytes.get()[start..start + n * FIXED]
    }

    /// Where `d` is in fixed records `r` (sorted by digest): interpolation
    /// search, the digests being uniform (a few probes for any size), with
    /// a bisection after any step that did not halve the interval, so no
    /// input costs more than twice a binary search.
    fn find_fixed(r: &[u8], d: &[u8; 32]) -> Option<u64> {
        let key = |i: usize| -> &[u8] { &r[i * FIXED..i * FIXED + 32] };
        let prefix = |k: &[u8]| u64::from_be_bytes(k[..8].try_into().unwrap()) as f64;
        let t = prefix(d);
        let (mut lo, mut hi) = (0usize, r.len() / FIXED);
        let (mut lo_k, mut hi_k) = (0f64, 18446744073709551616f64);
        let mut bisect = false;
        while lo < hi {
            let size = hi - lo;
            let mid = if bisect || hi_k <= lo_k {
                lo + size / 2
            } else {
                let est = lo as f64 + (t - lo_k) / (hi_k - lo_k) * size as f64;
                (est as usize).clamp(lo, hi - 1)
            };
            let k = key(mid);
            match k.cmp(&d[..]) {
                std::cmp::Ordering::Equal => {
                    let o = &r[mid * FIXED + 32..(mid + 1) * FIXED];
                    return Some(u64::from_be_bytes(o.try_into().unwrap()));
                }
                std::cmp::Ordering::Less => (lo, lo_k) = (mid + 1, prefix(k)),
                std::cmp::Ordering::Greater => (hi, hi_k) = (mid, prefix(k)),
            }
            bisect = (hi - lo) * 2 > size;
        }
        None
    }

    /// Version 2 record `i`'s name bytes; a record pointing outside the
    /// file reads as empty (and so never matches).
    fn name_bytes(&self, i: usize) -> &[u8] {
        let b = &self.bytes.get()[self.base..];
        let r = &b[12 + i * REC..12 + (i + 1) * REC];
        let start = u32::from_be_bytes(r[..4].try_into().unwrap()) as usize;
        let len = u16::from_be_bytes(r[4..6].try_into().unwrap()) as usize;
        b.get(start..start + len).unwrap_or_default()
    }

    fn name(&self, i: usize) -> io::Result<&str> {
        std::str::from_utf8(self.name_bytes(i)).map_err(bad)
    }

    fn offset(&self, i: usize) -> u64 {
        let r = &self.bytes.get()[self.base + 12 + i * REC..self.base + 12 + (i + 1) * REC];
        u64::from_be_bytes(r[6..].try_into().unwrap())
    }

    /// The first version 2 record whose name is not less than `name`.
    fn lower_bound(&self, name: &[u8]) -> usize {
        let (mut lo, mut hi) = (0, self.strs);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.name_bytes(mid) < name {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    fn get(&self, name: &str) -> Option<u64> {
        if let Some((s, d)) = fixed_name(name) {
            let r = self.fixed(s);
            let fan = match self.fans[s].get() {
                Some(f) => Some(f),
                None if self.fixed_searches.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= HASH_AFTER => {
                    Some(self.fans[s].get_or_init(|| Fan::build(r)))
                }
                None => None,
            };
            let found = match fan {
                Some(f) => {
                    let (lo, hi) = f.range(&d);
                    Idx::find_fixed(&r[lo * FIXED..hi * FIXED], &d)
                }
                None => Idx::find_fixed(r, &d),
            };
            if let Some(o) = found {
                return Some(o);
            }
            // A version 2 file holds keys among its strings.
            if self.base != 0 {
                return None;
            }
        }
        let name = name.as_bytes();
        let table = match self.table.get() {
            Some(t) => Some(t),
            None if self.searches.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= HASH_AFTER => {
                Some(self.table.get_or_init(|| self.build_table()))
            }
            None => None,
        };
        let i = match table {
            Some(t) => {
                // Open addressing, a power of two at least twice n; slot
                // values are record + 1, 0 empty.
                let mask = t.len() - 1;
                let mut at = name_hash(name) as usize & mask;
                loop {
                    match t[at] {
                        0 => return None,
                        r if self.name_bytes(r as usize - 1) == name => break r as usize - 1,
                        _ => at = (at + 1) & mask,
                    }
                }
            }
            None => self.lower_bound(name),
        };
        (i < self.strs && self.name_bytes(i) == name).then(|| self.offset(i))
    }

    fn build_table(&self) -> Vec<u32> {
        let size = (self.strs * 2).next_power_of_two().max(2);
        let mut t = vec![0u32; size];
        let mask = size - 1;
        for i in 0..self.strs {
            let mut at = name_hash(self.name_bytes(i)) as usize & mask;
            while t[at] != 0 {
                at = (at + 1) & mask;
            }
            t[at] = i as u32 + 1;
        }
        t
    }

    /// The names not less than `from` with their offsets, in name order.
    fn entries_from<'a>(&'a self, from: &str) -> impl Iterator<Item = io::Result<(String, u64)>> + 'a {
        // Each section is sorted, and so is each as strings (a common
        // prefix, then lowercase hex); merge the three.
        let mut fixed: Vec<(usize, usize)> = (0..SECTIONS.len())
            .map(|s| {
                let (p, r) = (SECTIONS[s], self.fixed(s));
                let n = r.len() / FIXED;
                let start = if from <= p {
                    0
                } else if let Some(rest) = from.strip_prefix(p) {
                    // The first record whose hex is not less than rest.
                    let (mut lo, mut hi) = (0, n);
                    while lo < hi {
                        let mid = (lo + hi) / 2;
                        if hex::encode(&r[mid * FIXED..mid * FIXED + 32]).as_str() < rest {
                            lo = mid + 1;
                        } else {
                            hi = mid;
                        }
                    }
                    lo
                } else {
                    n
                };
                (start, n)
            })
            .collect();
        let mut str_at = self.lower_bound(from.as_bytes());
        let mut heads: [Option<(String, u64)>; 2] = [None, None];
        std::iter::from_fn(move || {
            for s in 0..SECTIONS.len() {
                if heads[s].is_none() && fixed[s].0 < fixed[s].1 {
                    let i = fixed[s].0;
                    let rec = &self.fixed(s)[i * FIXED..(i + 1) * FIXED];
                    heads[s] = Some((
                        format!("{}{}", SECTIONS[s], hex::encode(&rec[..32])),
                        u64::from_be_bytes(rec[32..].try_into().unwrap()),
                    ));
                    fixed[s].0 += 1;
                }
            }
            let str_head = (str_at < self.strs).then(|| self.name(str_at));
            let str_head = match str_head {
                Some(Err(e)) => return Some(Err(e)),
                Some(Ok(n)) => Some(n),
                None => None,
            };
            // The least of the heads.
            let mut best: Option<usize> = None;
            for s in 0..SECTIONS.len() {
                if let Some((n, _)) = &heads[s] {
                    let better = match best {
                        None => str_head.is_none_or(|h| n.as_str() < h),
                        Some(b) => n < &heads[b].as_ref().unwrap().0,
                    };
                    if better {
                        best = Some(s);
                    }
                }
            }
            match best {
                Some(s) => heads[s].take().map(Ok),
                None => {
                    let n = str_head?.to_string();
                    let o = self.offset(str_at);
                    str_at += 1;
                    Some(Ok((n, o)))
                }
            }
        })
    }
}

/// A version 3 index of `entries` (name, frame offset).
fn encode_idx(mut entries: Vec<(String, u64)>) -> Vec<u8> {
    entries.sort();
    entries.dedup_by(|a, b| a.0 == b.0);
    let mut fixed: [Vec<([u8; 32], u64)>; 2] = [Vec::new(), Vec::new()];
    let mut strs = Vec::new();
    for (name, off) in entries {
        match fixed_name(&name) {
            Some((s, d)) => fixed[s].push((d, off)),
            None => strs.push((name, off)),
        }
    }
    let mut out = IDX_MAGIC.to_vec();
    for f in &fixed {
        out.extend_from_slice(&(f.len() as u32).to_be_bytes());
    }
    for f in &mut fixed {
        // Sorted by name is sorted by digest already; keep it explicit.
        f.sort();
        for (d, off) in f.iter() {
            out.extend_from_slice(d);
            out.extend_from_slice(&off.to_be_bytes());
        }
    }
    out.extend_from_slice(&encode_v2(&strs));
    out
}

/// A version 2 index of sorted, distinct `entries`.
fn encode_v2(entries: &[(String, u64)]) -> Vec<u8> {
    let mut out = IDX_MAGIC_V2.to_vec();
    out.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    let mut at = 12 + entries.len() * REC;
    for (name, off) in entries {
        out.extend_from_slice(&(at as u32).to_be_bytes());
        out.extend_from_slice(&(name.len() as u16).to_be_bytes());
        out.extend_from_slice(&off.to_be_bytes());
        at += name.len();
    }
    for (name, _) in entries {
        out.extend_from_slice(name.as_bytes());
    }
    out
}

/// A frame found: where it starts, in which mapping, under which index.
#[derive(Clone)]
struct Mapped {
    offset: u64,
    map: std::sync::Arc<Map>,
    idx: std::sync::Arc<Idx>,
}

impl Mapped {
    /// The frame of the same pack starting at `offset`.
    fn at(&self, offset: u64) -> Mapped {
        Mapped { offset, ..self.clone() }
    }

    fn key(&self) -> (u64, u64) {
        (self.map.uid, self.offset)
    }

    /// Whether this frame is a delta, its kind, its data, and its length.
    fn parse(&self) -> io::Result<(bool, u8, &[u8], usize)> {
        let b = self.map.bytes().get(self.offset as usize..).ok_or_else(|| bad("offset past the pack"))?;
        parse_frame(b)
    }

    /// The base of the delta frame `data` of kind `kind` (this frame's).
    fn base(&self, packs: &Packs, kind: u8, data: &[u8]) -> io::Result<Option<Mapped>> {
        match delta_parts(data, kind)?.0 {
            Base::Back(d) => {
                let o = self.offset.checked_sub(d).filter(|_| d > 0).ok_or_else(|| bad("delta base offset"))?;
                Ok(Some(self.at(o)))
            }
            // A base may have been removed: its dependents still need it.
            Base::Name(n) => packs.locate_mapped(&n, true),
        }
    }

    fn frame(&self) -> io::Result<Frame> {
        let (delta, kind, data, len) = self.parse()?;
        let b = &self.map.bytes()[self.offset as usize..];
        let bytes = b[..len].to_vec();
        let data_at = len - data.len();
        if delta {
            let base = match delta_parts(data, kind)?.0 {
                Base::Name(n) => n,
                Base::Back(d) => {
                    self.idx.name_at(self.offset.checked_sub(d).ok_or_else(|| bad("delta base offset"))?)?
                }
            };
            Ok(Frame { bytes, data_at, base: Some(base), plain: true })
        } else {
            Ok(Frame { bytes, data_at, base: None, plain: matches!(data.first(), Some(0 | 1)) })
        }
    }
}

#[derive(Default)]
struct State {
    /// Pack ids, in load order; `Loc::pack` indexes this, `idx` and `maps`.
    packs: Vec<String>,
    idx: Vec<std::sync::Arc<Idx>>,
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

/// The size from which [`Packs::compact`] leaves a pack alone.
pub const BIG_PACK: u64 = 64 << 20;

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
const BASE_CACHE_BYTES: usize = 256 << 20;

/// A cached value and when it was last used.
type Cached = (std::sync::Arc<Vec<u8>>, u64);

#[derive(Default)]
struct BaseCache {
    /// By (mapping, frame offset).
    map: std::collections::HashMap<(u64, u64), Cached>,
    bytes: usize,
    tick: u64,
}

impl BaseCache {
    fn get(&mut self, at: (u64, u64)) -> Option<std::sync::Arc<Vec<u8>>> {
        self.tick += 1;
        let t = self.tick;
        self.map.get_mut(&at).map(|(v, used)| {
            *used = t;
            v.clone()
        })
    }

    fn put(&mut self, at: (u64, u64), v: std::sync::Arc<Vec<u8>>) {
        if v.len() > BASE_CACHE_BYTES / 4 {
            return;
        }
        self.tick += 1;
        self.bytes += v.len();
        if let Some((old, _)) = self.map.insert(at, (v, self.tick)) {
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

/// An unsigned LEB128 varint.
fn put_varint(out: &mut Vec<u8>, mut n: usize) {
    while n >= 0x80 {
        out.push(n as u8 | 0x80);
        n >>= 7;
    }
    out.push(n as u8);
}

fn take_varint(b: &mut &[u8]) -> io::Result<usize> {
    let mut n = 0usize;
    for shift in (0..64).step_by(7) {
        let c = take(b, 1)?[0];
        n |= ((c & 0x7f) as usize) << shift;
        if c < 0x80 {
            return Ok(n);
        }
    }
    Err(bad("varint too long"))
}

fn take_u32(b: &mut &[u8]) -> io::Result<usize> {
    Ok(u32::from_be_bytes(take(b, 4)?.try_into().unwrap()) as usize)
}

fn put_name(out: &mut Vec<u8>, name: &str) {
    out.extend_from_slice(&(name.len() as u16).to_be_bytes());
    out.extend_from_slice(name.as_bytes());
}

/// A name in binary form: `1` and the 32 digest bytes of a `blake3:` key,
/// `2` and those of a chunk (`#hex`), else `0` and the name as
/// [`put_name`] writes it. A blake3 key takes 33 bytes, not 73.
fn put_bin_name(out: &mut Vec<u8>, name: &str) {
    if let Some(d) = name.strip_prefix("blake3:").and_then(hex32) {
        out.push(1);
        out.extend_from_slice(&d);
    } else if let Some(d) = name.strip_prefix('#').and_then(hex32) {
        out.push(2);
        out.extend_from_slice(&d);
    } else {
        out.push(0);
        put_name(out, name);
    }
}

/// A name written by [`put_name`] (`binary` false) or [`put_bin_name`].
fn take_any_name(b: &mut &[u8], binary: bool) -> io::Result<String> {
    if !binary {
        let n = take_u16(b)?;
        return std::str::from_utf8(take(b, n)?).map(str::to_string).map_err(bad);
    }
    match take(b, 1)?[0] {
        0 => take_any_name(b, false),
        1 => Ok(format!("blake3:{}", hex::encode(take(b, 32)?))),
        2 => Ok(format!("#{}", hex::encode(take(b, 32)?))),
        t => Err(bad(format!("name tag {t}"))),
    }
}

/// Skip a name as [`take_any_name`] reads it.
fn skip_name(b: &mut &[u8], binary: bool) -> io::Result<()> {
    if !binary {
        let n = take_u16(b)?;
        return take(b, n).map(|_| ());
    }
    match take(b, 1)?[0] {
        0 => skip_name(b, false),
        1 | 2 => take(b, 32).map(|_| ()),
        t => Err(bad(format!("name tag {t}"))),
    }
}

/// One frame at the start of `b`: whether it is a delta, whether its names
/// are binary, its data, and its whole length.
fn parse_frame(mut b: &[u8]) -> io::Result<(bool, u8, &[u8], usize)> {
    let start = b.len();
    let kind = take(&mut b, 1)?[0];
    let delta = match kind {
        FILE | FILE_B | FILE_N => false,
        DELTA | DELTA_B | DELTA_N | DELTA_O => true,
        k => return Err(bad(format!("frame kind {k}"))),
    };
    let n = match kind {
        FILE_N | DELTA_N | DELTA_O => take_varint(&mut b)?,
        _ => {
            skip_name(&mut b, kind >= FILE_B)?;
            take_u32(&mut b)?
        }
    };
    let data = take(&mut b, n)?;
    Ok((delta, kind, data, start - b.len()))
}

/// A delta's base: by name, or by how far back in the pack its frame starts.
enum Base {
    Name(String),
    Back(u64),
}

/// A delta frame's data, by the frame's kind: its base, value length and
/// zstd bytes.
fn delta_parts(mut d: &[u8], kind: u8) -> io::Result<(Base, usize, &[u8])> {
    let base = match kind {
        DELTA_O => Base::Back(take_varint(&mut d)? as u64),
        _ => Base::Name(take_any_name(&mut d, kind != DELTA)?),
    };
    let len = if matches!(kind, DELTA_N | DELTA_O) { take_varint(&mut d)? } else { take_u32(&mut d)? };
    Ok((base, len, d))
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

/// One zstd frame `z` decoded to exactly `len` bytes, with `dict` as a
/// raw-content dictionary if given.
pub fn zstd_exact(dict: Option<&[u8]>, z: &[u8], len: usize) -> io::Result<Vec<u8>> {
    match dict {
        Some(d) => with_dict_decompress(d, z, len),
        None => {
            let mut out = Vec::with_capacity(len);
            DCTX.with(|c| c.borrow_mut().decompress(&mut out, z)).map_err(zerr)?;
            if out.len() != len {
                return Err(bad("zstd frame decodes to the wrong size"));
            }
            Ok(out)
        }
    }
}

/// `value` as one zstd frame at level 3, with this thread's context
/// (making a context per value cost as much as compressing a small one).
pub(crate) fn zstd3(value: &[u8]) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(zstd::zstd_safe::compress_bound(value.len()));
    CCTX.with(|c| c.borrow_mut().compress(&mut out, value, 3)).map_err(zerr)?;
    Ok(out)
}

fn with_dict_compress(dict: &[u8], value: &[u8]) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(zstd::zstd_safe::compress_bound(value.len()));
    CCTX.with(|c| c.borrow_mut().compress_using_dict(&mut out, value, dict, 3)).map_err(zerr)?;
    Ok(out)
}

impl Frame {
    /// A delta frame's base, value length and zstd bytes.
    pub fn delta(&self) -> io::Result<Option<(String, usize, &[u8])>> {
        let Some(base) = &self.base else { return Ok(None) };
        let (_, len, z) = delta_parts(&self.bytes[self.data_at..], self.bytes[0])?;
        Ok(Some((base.clone(), len, z)))
    }
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
    /// Where each name's frame starts, for offset deltas on it.
    at: crate::keyhash::KeyMap<u64>,
}

impl PackWriter<'_> {
    /// Copy a frame as stored in another pack under `name`: a whole frame
    /// byte for byte, a delta written again (its base by distance when it
    /// is in this pack already, else by name).
    pub fn add_frame(&mut self, name: &str, f: &Frame) -> io::Result<()> {
        match f.delta()? {
            Some((base, len, z)) => self.add_delta(name, &base, len, z),
            None => {
                self.start(name);
                self.out.write_all(&f.bytes)?;
                self.offset += f.bytes.len() as u64;
                Ok(())
            }
        }
    }

    fn start(&mut self, name: &str) {
        self.idx.push((name.to_string(), self.offset));
        self.at.insert(name.to_string(), self.offset);
    }

    fn add_delta(&mut self, name: &str, base: &str, len: usize, z: &[u8]) -> io::Result<()> {
        let mut d = Vec::new();
        let kind = match self.at.get(base) {
            Some(&b) => {
                put_varint(&mut d, (self.offset - b) as usize);
                DELTA_O
            }
            None => {
                put_bin_name(&mut d, base);
                DELTA_N
            }
        };
        put_varint(&mut d, len);
        let mut frame = vec![kind];
        put_varint(&mut frame, d.len() + z.len());
        self.start(name);
        self.out.write_all(&frame)?;
        self.out.write_all(&d)?;
        self.out.write_all(z)?;
        self.offset += (frame.len() + d.len() + z.len()) as u64;
        Ok(())
    }

    pub fn add(&mut self, name: &str, e: &Entry) -> io::Result<()> {
        match e {
            Entry::File(bytes) => {
                let mut frame = vec![FILE_N];
                put_varint(&mut frame, bytes.len());
                self.start(name);
                self.out.write_all(&frame)?;
                self.out.write_all(bytes)?;
                self.offset += (frame.len() + bytes.len()) as u64;
                Ok(())
            }
            Entry::Delta { base, len, z } => self.add_delta(name, base, *len, z),
        }
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
            let idx = Idx::open(&id, &self.dir.join(format!("{id}.idx")))?;
            st.idx.push(std::sync::Arc::new(idx));
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
    fn locate_mapped(&self, name: &str, with_removed: bool) -> io::Result<Option<Mapped>> {
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
    ) -> io::Result<Option<Mapped>> {
        let Some(loc) = find(&st) else { return Ok(None) };
        let cell = &st.maps[loc.pack];
        let idx = st.idx[loc.pack].clone();
        if let Some(m) = cell.get() {
            return Ok(Some(Mapped { offset: loc.offset, map: m.clone(), idx }));
        }
        match Map::open(&self.dir.join(format!("{}.pack", st.packs[loc.pack]))) {
            Ok(m) => {
                Ok(Some(Mapped { offset: loc.offset, map: cell.get_or_init(|| std::sync::Arc::new(m)).clone(), idx }))
            }
            // Merged away by a compaction since the last look: look again.
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                drop(st);
                self.state.write().unwrap().seen_mtime = None;
                self.refresh()?;
                let st = self.state.read().unwrap();
                let Some(loc) = find(&st) else { return Ok(None) };
                let m = Map::open(&self.dir.join(format!("{}.pack", st.packs[loc.pack])))?;
                let map = st.maps[loc.pack].get_or_init(|| std::sync::Arc::new(m)).clone();
                Ok(Some(Mapped { offset: loc.offset, map, idx: st.idx[loc.pack].clone() }))
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

    /// [`Packs::contains`] of each name, under one fresh look.
    pub fn contains_many(&self, names: &[&str]) -> io::Result<Vec<bool>> {
        self.look(true)?;
        let st = self.state.read().unwrap();
        Ok(names.iter().map(|n| !st.removed.contains(*n) && st.get(n).is_some()).collect())
    }

    /// The file bytes stored for `name` (deltas resolved to a `0`-header file).
    pub fn file(&self, name: &str) -> io::Result<Option<Vec<u8>>> {
        let Some(m) = self.locate_mapped(name, false)? else { return Ok(None) };
        let as_file = |v: &[u8]| {
            let mut f = Vec::with_capacity(v.len() + 1);
            f.push(0);
            f.extend_from_slice(v);
            f
        };
        // A value decoded lately as a base (a walk often reads a chain from
        // its newest end) is not decoded again.
        if let Some(v) = self.bases.lock().unwrap().get(m.key()) {
            return Ok(Some(as_file(&v)));
        }
        match m.parse()? {
            (false, _, data, _) => Ok(Some(data.to_vec())),
            (true, kind, data, _) => {
                let v = self.delta_value(&m, kind, data, 0)?;
                Ok(Some(as_file(&v)))
            }
        }
    }

    /// The value of the delta frame at `m` (of `kind`, with `data`), kept
    /// in the base cache: a walk reads the next version at the same path
    /// soon, a delta on this one.
    fn delta_value(&self, m: &Mapped, kind: u8, data: &[u8], depth: usize) -> io::Result<std::sync::Arc<Vec<u8>>> {
        let (_, size, z) = delta_parts(data, kind)?;
        let base = m.base(self, kind, data)?.ok_or_else(|| bad("delta base gone"))?;
        let base_value = self.base_value(&base, depth + 1)?;
        let v = std::sync::Arc::new(with_dict_decompress(&base_value, z, size)?);
        self.bases.lock().unwrap().put(m.key(), v.clone());
        Ok(v)
    }

    /// The plain value of the frame at `m`, a delta base, from the cache if
    /// there.
    fn base_value(&self, m: &Mapped, depth: usize) -> io::Result<std::sync::Arc<Vec<u8>>> {
        if let Some(v) = self.bases.lock().unwrap().get(m.key()) {
            return Ok(v);
        }
        if depth > MAX_DEPTH + 1 {
            return Err(bad("delta chain too long"));
        }
        match m.parse()? {
            (false, _, data, _) => {
                let v = std::sync::Arc::new(plain_value(data)?);
                self.bases.lock().unwrap().put(m.key(), v.clone());
                Ok(v)
            }
            (true, kind, data, _) => self.delta_value(m, kind, data, depth),
        }
    }

    /// `f` of a packed `name`'s file data (`None` for a delta), read in
    /// place: for scans that look at one byte of every blob.
    pub fn peek<R>(&self, name: &str, f: impl FnOnce(Option<&[u8]>) -> io::Result<R>) -> io::Result<Option<R>> {
        let Some(m) = self.locate_mapped(name, false)? else { return Ok(None) };
        match m.parse()? {
            (false, _, data, _) => f(Some(data)).map(Some),
            (true, ..) => f(None).map(Some),
        }
    }

    /// The frame stored for `name`, as it is in its pack, to copy into
    /// another pack (a repack reusing deltas, as git does).
    pub fn frame(&self, name: &str) -> io::Result<Option<Frame>> {
        let Some(m) = self.locate_mapped(name, false)? else { return Ok(None) };
        m.frame().map(Some)
    }

    /// The frame of `name` in the sealed pack, if it is there and not removed.
    pub fn sealed_frame(&self, name: &str) -> io::Result<Option<Frame>> {
        self.look(false)?;
        let st = self.state.read().unwrap();
        if st.removed.contains(name) {
            return Ok(None);
        }
        let find = |st: &State| st.sealed.and_then(|p| st.idx[p].get(name).map(|offset| Loc { pack: p, offset }));
        let Some(m) = self.mapped(st, find)? else { return Ok(None) };
        m.frame().map(Some)
    }

    /// The delta chain depth of `name` as a base: 0 for a plain whole
    /// frame, `None` when it is not packed or cannot be a base. Bases by
    /// name must not be removed.
    pub fn depth(&self, name: &str) -> io::Result<Option<usize>> {
        let Some(mut m) = self.locate_mapped(name, false)? else { return Ok(None) };
        for d in 0..=MAX_DEPTH {
            let (delta, kind, data, _) = m.parse()?;
            if !delta {
                return Ok(matches!(data.first(), Some(0 | 1)).then_some(d));
            }
            m = match delta_parts(data, kind)?.0 {
                Base::Back(_) => m.base(self, kind, data)?.unwrap(),
                Base::Name(n) => match self.locate_mapped(&n, false)? {
                    Some(b) => b,
                    None => return Ok(None),
                },
            };
        }
        Ok(None)
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

impl Packs {
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
            for e in idx.entries_from(from) {
                let (k, _) = e?;
                if to.is_some_and(|t| k.as_str() >= t) || taken >= limit {
                    break;
                }
                if !st.removed.contains(&k) {
                    out.insert(k);
                    taken += 1;
                }
            }
        }
        Ok(out.into_iter().take(limit).collect())
    }

    /// Keep the number of packs logarithmic, like git's geometric repack:
    /// packs are in age order, and the newest ones are merged into one
    /// while the pack before them holds at most as many entries as they do
    /// together (a binary counter: after n equal batches the packs hold the
    /// binary digits of n). Each entry is then copied at most log2 n times
    /// in all, and a lookup searches O(log n) packs. A factor of 2 instead
    /// copied two thirds more during a clone.
    /// A pack of [`BIG_PACK`] bytes or more is left as it is (a clone writes
    /// a few such; gc's repack rewrites them): merging them saved no lookup
    /// worth a copy of the store.
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
            st.packs.iter().cloned().zip(st.idx.iter().map(|i| i.len())).unzip()
        };
        let Some(last) = ids.len().checked_sub(1) else { return Ok(()) };
        let big = |id: &str| fs::metadata(self.dir.join(format!("{id}.pack"))).is_ok_and(|m| m.len() >= BIG_PACK);
        if big(&ids[last]) {
            return Ok(());
        }
        let (mut first, mut acc) = (last, sizes[last]);
        while first > 0 && sizes[first - 1] <= acc && !big(&ids[first - 1]) {
            first -= 1;
            acc += sizes[first];
        }
        if first == last {
            return Ok(());
        }
        self.merge(&ids[first..], tmp)?;
        Ok(())
    }

    /// Merge every pack into one and record it as the sealed pack, as a
    /// repack would: for a writer whose deltas are as good as a repack's
    /// (git import, against each path's last version), so the next gc
    /// reuses them instead of trying each again. Best effort, like
    /// [`Packs::compact`].
    pub fn seal_all(&self, tmp: &Path) -> io::Result<()> {
        use std::os::fd::AsRawFd;
        let lock = File::create(self.dir.join("compact.lock"))?;
        // SAFETY: flock on a descriptor we own; released when it closes.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Ok(());
        }
        self.refresh()?;
        let ids = self.state.read().unwrap().packs.clone();
        let id = match ids.as_slice() {
            [] => return Ok(()),
            [one] => one.clone(),
            _ => self.merge(&ids, tmp)?,
        };
        self.seal(&id, tmp)
    }

    /// Copy the frames of packs `merge` (oldest first; a name's first frame
    /// wins) into one new pack, then drop them. The new pack's id.
    fn merge(&self, merge: &[String], tmp: &Path) -> io::Result<String> {
        let mut w = self.writer(tmp)?;
        let mut seen = std::collections::HashSet::new();
        for id in merge {
            let idx = std::sync::Arc::new(Idx::open(id, &self.dir.join(format!("{id}.idx")))?);
            let map = std::sync::Arc::new(Map::open(&self.dir.join(format!("{id}.pack")))?);
            // In the pack's order: an offset delta's base comes first, so
            // it stays an offset delta.
            let m = Mapped { offset: 0, map, idx: idx.clone() };
            for &(at, i) in idx.by_offset() {
                let (name, _) = idx.record(i as usize)?;
                if !seen.insert(name.clone()) {
                    continue;
                }
                w.add_frame(&name, &m.at(at).frame()?)?;
            }
        }
        let new = w.finish()?;
        for id in merge {
            let _ = fs::remove_file(self.dir.join(format!("{id}.idx")));
            let _ = fs::remove_file(self.dir.join(format!("{id}.pack")));
        }
        File::open(&self.dir)?.sync_all()?;
        self.refresh()?;
        Ok(new)
    }

    /// Start a pack; temp files go in `tmp`.
    pub fn writer(&self, tmp: &Path) -> io::Result<PackWriter<'_>> {
        let id = format!(
            "{:020}-{}-{}",
            SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_nanos()),
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let mut out = io::BufWriter::with_capacity(1 << 20, File::create(tmp.join(format!("{id}.pack")))?);
        out.write_all(PACK_MAGIC)?;
        Ok(PackWriter {
            packs: self,
            id,
            tmp: tmp.to_path_buf(),
            out,
            offset: PACK_MAGIC.len() as u64,
            idx: Vec::new(),
            at: Default::default(),
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
        *self.bases.lock().unwrap() = BaseCache::default();
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
        assert_eq!(idx.entries_from("b").next().unwrap().unwrap().0, "m");
    }

    /// Frames with string names (kinds 0 and 1, written before binary
    /// names) still read, alongside binary ones, deltas across both.
    #[test]
    fn frames_with_string_names_still_read() {
        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path().join("tmp");
        fs::create_dir_all(&tmp).unwrap();
        let p = Packs::open(dir.path().join("packs")).unwrap();
        let (a, b, c) = (format!("blake3:{:064x}", 1), format!("blake3:{:064x}", 2), "odd-name".to_string());
        let base = b"\0line one\nline two\n".to_vec();
        let value = b"line one\nline two\nline three\n".to_vec();
        // An old pack by hand: a file and a delta on it, string names.
        let mut pack = PACK_MAGIC.to_vec();
        let mut idx = Vec::new();
        idx.push((a.clone(), pack.len() as u64));
        pack.push(FILE);
        put_name(&mut pack, &a);
        pack.extend_from_slice(&(base.len() as u32).to_be_bytes());
        pack.extend_from_slice(&base);
        let z = with_dict_compress(&base[1..], &value).unwrap();
        let mut d = Vec::new();
        put_name(&mut d, &a);
        d.extend_from_slice(&(value.len() as u32).to_be_bytes());
        d.extend_from_slice(&z);
        idx.push((b.clone(), pack.len() as u64));
        pack.push(DELTA);
        put_name(&mut pack, &b);
        pack.extend_from_slice(&(d.len() as u32).to_be_bytes());
        pack.extend_from_slice(&d);
        // And a kind 3 frame (binary names, u32 lengths) on the same base.
        let d3 = format!("blake3:{:064x}", 3);
        let mut d = Vec::new();
        put_bin_name(&mut d, &a);
        d.extend_from_slice(&(value.len() as u32).to_be_bytes());
        d.extend_from_slice(&z);
        idx.push((d3.clone(), pack.len() as u64));
        pack.push(DELTA_B);
        put_bin_name(&mut pack, &d3);
        pack.extend_from_slice(&(d.len() as u32).to_be_bytes());
        pack.extend_from_slice(&d);
        fs::write(dir.path().join("packs/1.pack"), &pack).unwrap();
        fs::write(dir.path().join("packs/1.idx"), encode_idx(idx)).unwrap();
        // A new pack: a binary-named delta on the old file, and a name
        // that is not a key.
        p.write(&[(c.clone(), Entry::Delta { base: a.clone(), len: value.len(), z: z.clone() })], &tmp).unwrap();
        let other = Packs::open(dir.path().join("packs")).unwrap();
        let want = [&[0u8][..], &value].concat();
        assert_eq!(other.file(&a).unwrap(), Some(base.clone()));
        assert_eq!(other.file(&b).unwrap(), Some(want.clone()));
        assert_eq!(other.file(&c).unwrap(), Some(want.clone()));
        assert_eq!(other.file(&d3).unwrap(), Some(want.clone()));
        assert_eq!(other.frame(&d3).unwrap().unwrap().delta().unwrap().unwrap().0, a);
        assert_eq!(other.frame(&b).unwrap().unwrap().base.as_deref(), Some(a.as_str()));
        assert_eq!(other.frame(&c).unwrap().unwrap().delta().unwrap().unwrap().0, a);
        // Merging copies both kinds as they are.
        other.seal_all(&tmp).unwrap();
        let merged = Packs::open(dir.path().join("packs")).unwrap();
        assert_eq!(merged.ids().unwrap().len(), 1);
        for (n, v) in [(&a, &base), (&b, &want), (&c, &want), (&d3, &want)] {
            assert_eq!(merged.file(n).unwrap().as_ref(), Some(v));
        }
    }

    /// A delta on a base written before it in the same pack names the base
    /// by distance; one on a base in another pack by name, until a merge
    /// puts both in one pack. Chains read, depths count, frames name their
    /// bases either way.
    #[test]
    fn deltas_on_bases_in_the_same_pack_name_them_by_offset() {
        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path().join("tmp");
        fs::create_dir_all(&tmp).unwrap();
        let p = Packs::open(dir.path().join("packs")).unwrap();
        let key = |i: u32| format!("blake3:{:064x}", i);
        let vals: Vec<Vec<u8>> = (0..4).map(|i| format!("line one\nline two\nversion {i}\n").into_bytes()).collect();
        let delta = |i: usize| {
            let z = with_dict_compress(&vals[i - 1], &vals[i]).unwrap();
            Entry::Delta { base: key(i as u32 - 1), len: vals[i].len(), z }
        };
        let whole = [&[0u8][..], &vals[0]].concat();
        p.write(&[(key(0), Entry::File(whole)), (key(1), delta(1)), (key(2), delta(2))], &tmp).unwrap();
        p.write(&[(key(3), delta(3))], &tmp).unwrap();
        let kinds = |p: &Packs| -> Vec<u8> {
            (0..4).map(|i| p.locate_mapped(&key(i), false).unwrap().unwrap().parse().unwrap().1).collect()
        };
        assert_eq!(kinds(&p), [FILE_N, DELTA_O, DELTA_O, DELTA_N]);
        let check = |p: &Packs| {
            for (i, v) in vals.iter().enumerate() {
                let want = [&[0u8][..], v].concat();
                assert_eq!(p.file(&key(i as u32)).unwrap(), Some(want), "{i}");
                assert_eq!(p.depth(&key(i as u32)).unwrap(), Some(i));
                let base = p.frame(&key(i as u32)).unwrap().unwrap().base;
                assert_eq!(base, (i > 0).then(|| key(i as u32 - 1)));
            }
        };
        check(&p);
        // A fresh process (no base cache) reads the same.
        check(&Packs::open(dir.path().join("packs")).unwrap());
        p.seal_all(&tmp).unwrap();
        let merged = Packs::open(dir.path().join("packs")).unwrap();
        assert_eq!(merged.ids().unwrap().len(), 1);
        assert_eq!(kinds(&merged), [FILE_N, DELTA_O, DELTA_O, DELTA_O]);
        check(&merged);
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

    /// Keys and chunk names go in fixed digest sections, other names in
    /// the string part; lookups and ordered listing agree with a sorted
    /// list of every name, and a version 2 index of them still reads.
    #[test]
    fn a_version_3_index_matches_a_sorted_list() {
        let mut names: Vec<String> = (0..300u32)
            .map(|i| {
                let d = hex::encode(blake3::hash(&i.to_be_bytes()).as_bytes());
                match i % 3 {
                    0 => format!("blake3:{d}"),
                    1 => format!("#{d}"),
                    _ => format!("blake3:{}", d.to_uppercase()),
                }
            })
            .collect();
        names.extend(["a", "blake3:", "blake3:0", "blake3:zz", "#", "$", "z"].map(String::from));
        let entries: Vec<(String, u64)> = names.iter().enumerate().map(|(i, n)| (n.clone(), i as u64 * 7)).collect();
        let mut sorted = entries.clone();
        sorted.sort();
        let v3 = Idx::parse("x", encode_idx(entries.clone())).unwrap();
        let v2 = Idx::parse("x", encode_v2(&sorted)).unwrap();
        assert_eq!((v3.keys, v3.chunks, v3.strs), (100, 100, 107));
        for idx in [&v3, &v2] {
            for (n, o) in &entries {
                assert_eq!(idx.get(n), Some(*o), "{n}");
            }
            assert_eq!(idx.get(&format!("blake3:{}", "0".repeat(64))), None);
            let all: Vec<_> = idx.entries_from("").map(Result::unwrap).collect();
            assert_eq!(all, sorted);
            for from in ["", "#", "#8", "$", "a", "blake3:", "blake3:7f", "blake3:F", "blake3;", "zz"] {
                let got: Vec<_> = idx.entries_from(from).map(|e| e.unwrap().0).collect();
                let want: Vec<_> = sorted.iter().filter(|(n, _)| n.as_str() >= from).map(|(n, _)| n.clone()).collect();
                assert_eq!(got, want, "from {from}");
            }
        }
    }

    #[test]
    fn a_damaged_index_is_refused_or_matches_nothing() {
        assert!(Idx::parse("x", b"CASIDX02\0\0\0\x05".to_vec()).is_err());
        assert!(Idx::parse("x", b"CASIDX03\0\0\0\x01\0\0\0\0".to_vec()).is_err());
        let mut bytes = encode_idx(vec![("k".into(), 8)]);
        // The version 2 part starts at 16; its one record's name start.
        bytes[28..32].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(Idx::parse("x", bytes).unwrap().get("k"), None);
    }
}
