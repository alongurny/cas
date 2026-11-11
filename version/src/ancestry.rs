//! A history's ancestry in one compact blob (spec/functions/ancestry.md):
//! every node by a fixed-size record, sorted for binary search, with its
//! generation and predecessors by position. It answers merge-base walks
//! without reading the history's leaves, as git's commit-graph does.

use crate::derived::{self, Function};
use crate::history::{History, NodeId, Stored};
use crate::{Error, Result, View, malformed};
use cas_core::Store;
use cas_core::algo::{self, Algorithm};
use std::collections::BTreeMap;

const MAGIC: &[u8] = b"CASANCES\x01";

pub static ANCESTRY: Function = Function {
    name: "ancestry",
    input: "history",
    output: "raw",
    spec: include_str!("../../spec/functions/ancestry.md"),
    compute,
};

fn compute(store: &dyn Store, _: &'static Algorithm, input: &View) -> Result<Vec<u8>> {
    let h = crate::history::read(store, input)?;
    encode(&h).map_err(|e| malformed(input.key(), e))
}

/// An id split into its parts: (format key, version algorithm), the
/// version digest, the occurrence number (0 for none).
fn split(id: &str) -> Option<((&str, &str), Vec<u8>, u32)> {
    let (v, occ) = match id.rsplit_once('#') {
        Some((v, n)) => (v, n.parse::<u32>().ok()?),
        None => (id, 0),
    };
    let (format, key) = v.split_once(' ')?;
    let (alg, hex) = key.split_once(':')?;
    if hex.len() % 2 != 0 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let digest = (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect();
    Some(((format, alg), digest, occ))
}

/// The ancestry blob of `h`. Fails when its ids do not fit the format
/// (digests of different lengths, more than 65536 kinds).
pub fn encode(h: &History) -> std::result::Result<Vec<u8>, String> {
    let gens = crate::history::generations(h);
    let mut kinds: Vec<(&str, &str)> = Vec::new();
    let mut parts = Vec::with_capacity(h.nodes.len());
    let mut d = None;
    for id in h.nodes.keys() {
        let (kind, digest, occ) = split(id.as_str()).ok_or_else(|| format!("node id {id} has no compact form"))?;
        if *d.get_or_insert(digest.len()) != digest.len() {
            return Err("version digests of different lengths".into());
        }
        kinds.push(kind);
        parts.push((kind, digest, occ, id));
    }
    kinds.sort_unstable();
    kinds.dedup();
    if kinds.len() > 1 << 16 {
        return Err("too many kinds of id".into());
    }
    let d = d.unwrap_or(0);
    let mut recs: Vec<(Vec<u8>, &NodeId)> = parts
        .into_iter()
        .map(|(kind, digest, occ, id)| {
            let mut r = (kinds.binary_search(&kind).unwrap() as u16).to_be_bytes().to_vec();
            r.extend_from_slice(&digest);
            r.extend_from_slice(&occ.to_be_bytes());
            (r, id)
        })
        .collect();
    recs.sort_unstable();
    let pos: std::collections::HashMap<&NodeId, u32> =
        recs.iter().enumerate().map(|(i, (_, id))| (*id, i as u32)).collect();
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&(kinds.len() as u32).to_be_bytes());
    for (f, a) in &kinds {
        for s in [f, a] {
            out.extend_from_slice(&(s.len() as u16).to_be_bytes());
            out.extend_from_slice(s.as_bytes());
        }
    }
    out.push(d as u8);
    out.extend_from_slice(&(recs.len() as u32).to_be_bytes());
    // Columns: the digests, random, apart from what compresses.
    for (r, _) in &recs {
        out.extend_from_slice(&r[..2]);
    }
    for (r, _) in &recs {
        out.extend_from_slice(&r[2..2 + d]);
    }
    for (r, _) in &recs {
        out.extend_from_slice(&r[2 + d..]);
    }
    for (_, id) in &recs {
        out.extend_from_slice(&(gens[*id] as u32).to_be_bytes());
    }
    let mut edges = Vec::new();
    for (_, id) in &recs {
        out.extend_from_slice(&(edges.len() as u32).to_be_bytes());
        edges.extend(h.nodes[*id].after.keys().map(|p| pos[p]));
    }
    out.extend_from_slice(&(edges.len() as u32).to_be_bytes());
    for e in edges {
        out.extend_from_slice(&e.to_be_bytes());
    }
    Ok(out)
}

/// A decoded ancestry blob, read in place.
pub struct Ancestry {
    bytes: Vec<u8>,
    kinds: Vec<(String, String)>,
    d: usize,
    n: usize,
    /// Where each column starts.
    kind_at: usize,
    digest_at: usize,
    occ_at: usize,
    gen_at: usize,
    first_at: usize,
    edges_at: usize,
    ne: usize,
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}

impl Ancestry {
    fn kind(&self, i: usize) -> usize {
        usize::from(u16::from_be_bytes([self.bytes[self.kind_at + 2 * i], self.bytes[self.kind_at + 2 * i + 1]]))
    }

    fn digest(&self, i: usize) -> &[u8] {
        &self.bytes[self.digest_at + i * self.d..self.digest_at + (i + 1) * self.d]
    }

    fn occurrence(&self, i: usize) -> u32 {
        be32(&self.bytes, self.occ_at + 4 * i)
    }

    fn first(&self, i: usize) -> usize {
        if i == self.n { self.ne } else { be32(&self.bytes, self.first_at + 4 * i) as usize }
    }

    fn sort_key(&self, i: usize) -> (usize, &[u8], u32) {
        (self.kind(i), self.digest(i), self.occurrence(i))
    }

    /// Checks everything the spec requires: sorted unique records, kinds in
    /// range, predecessors in range and generations that fit them.
    pub fn decode(bytes: Vec<u8>) -> std::result::Result<Ancestry, String> {
        let b = &bytes;
        let take = |at: &mut usize, n: usize| -> std::result::Result<std::ops::Range<usize>, String> {
            let end = at.checked_add(n).filter(|&e| e <= b.len()).ok_or("truncated")?;
            let r = *at..end;
            *at = end;
            Ok(r)
        };
        let mut at = 0;
        if b.get(take(&mut at, MAGIC.len())?) != Some(MAGIC) {
            return Err("not an ancestry blob".into());
        }
        let nk = be32(b, take(&mut at, 4)?.start) as usize;
        let mut kinds = Vec::with_capacity(nk.min(b.len()));
        for _ in 0..nk {
            let mut two = [String::new(), String::new()];
            for s in &mut two {
                let l = u16::from_be_bytes(b[take(&mut at, 2)?].try_into().unwrap()) as usize;
                *s = String::from_utf8(b[take(&mut at, l)?].to_vec()).map_err(|_| "kind is not UTF-8")?;
            }
            let [f, a] = two;
            if !crate::is_blob_key(&f) || algo::lookup(&a).is_none() {
                return Err("bad kind".into());
            }
            kinds.push((f, a));
        }
        if kinds.windows(2).any(|w| w[0] >= w[1]) {
            return Err("kinds out of order".into());
        }
        let d = b[take(&mut at, 1)?.start] as usize;
        if kinds.iter().any(|(_, a)| algo::lookup(a).unwrap().digest_len != d) {
            return Err("digest length does not fit a kind".into());
        }
        let n = be32(b, take(&mut at, 4)?.start) as usize;
        let col = |at: &mut usize, w: usize| -> std::result::Result<usize, String> {
            Ok(take(at, n.checked_mul(w).ok_or("too many records")?)?.start)
        };
        let kind_at = col(&mut at, 2)?;
        let digest_at = col(&mut at, d)?;
        let occ_at = col(&mut at, 4)?;
        let gen_at = col(&mut at, 4)?;
        let first_at = col(&mut at, 4)?;
        let ne = be32(b, take(&mut at, 4)?.start) as usize;
        let edges_at = take(&mut at, ne.checked_mul(4).ok_or("too many edges")?)?.start;
        if at != b.len() {
            return Err("trailing bytes".into());
        }
        let a = Ancestry { kinds, d, n, kind_at, digest_at, occ_at, gen_at, first_at, edges_at, ne, bytes };
        if n > 0 && a.first(0) != 0 {
            return Err("predecessor lists out of place".into());
        }
        let col32 = |at: usize| a.bytes[at..at + 4 * n].as_chunks::<4>().0.iter().map(|c| u32::from_be_bytes(*c));
        let gens: Vec<u32> = col32(gen_at).collect();
        let firsts: Vec<usize> = col32(first_at).map(|f| f as usize).chain([ne]).collect();
        let edges: Vec<u32> =
            a.bytes[edges_at..edges_at + 4 * ne].as_chunks::<4>().0.iter().map(|c| u32::from_be_bytes(*c)).collect();
        let kinds_col = &a.bytes[kind_at..kind_at + 2 * n];
        for (i, occ) in col32(occ_at).enumerate() {
            let k = usize::from(u16::from_be_bytes([kinds_col[2 * i], kinds_col[2 * i + 1]]));
            if k >= a.kinds.len() {
                return Err("kind out of range".into());
            }
            if occ == 1 {
                return Err("occurrence 1".into());
            }
            if i > 0 {
                let pk = usize::from(u16::from_be_bytes([kinds_col[2 * i - 2], kinds_col[2 * i - 1]]));
                let ord = pk
                    .cmp(&k)
                    .then_with(|| a.digest(i - 1).cmp(a.digest(i)))
                    .then_with(|| a.occurrence(i - 1).cmp(&occ));
                if ord != std::cmp::Ordering::Less {
                    return Err("records out of order".into());
                }
            }
            let (f, to) = (firsts[i], firsts[i + 1]);
            if to < f || to > ne {
                return Err("predecessor lists out of place".into());
            }
            let mut want = 1u32;
            for &p in &edges[f..to] {
                let g = *gens.get(p as usize).ok_or("predecessor out of range")?;
                want = want.max(g.checked_add(1).ok_or("generation too large")?);
            }
            if gens[i] != want {
                return Err("a generation does not fit the predecessors".into());
            }
        }
        Ok(a)
    }

    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    pub fn generation(&self, i: usize) -> u32 {
        be32(&self.bytes, self.gen_at + 4 * i)
    }

    /// The positions of node `i`'s predecessors.
    pub fn after(&self, i: usize) -> impl Iterator<Item = usize> + '_ {
        (self.first(i)..self.first(i + 1)).map(move |e| be32(&self.bytes, self.edges_at + 4 * e) as usize)
    }

    /// The position of `id`, if the index holds it.
    pub fn find(&self, id: &NodeId) -> Option<usize> {
        let (kind, digest, occ) = split(id.as_str())?;
        let k = self.kinds.binary_search_by(|(f, a)| (f.as_str(), a.as_str()).cmp(&kind)).ok()?;
        let want = (k, &digest[..], occ);
        let (mut lo, mut hi) = (0, self.n);
        while lo < hi {
            let mid = (lo + hi) / 2;
            match self.sort_key(mid).cmp(&want) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(mid),
            }
        }
        None
    }

    /// Up to `limit` ids whose text starts with `<format> <prefix>`, in
    /// order, as [`Stored::with_key_prefix`]; `None` when `prefix` is not
    /// an algorithm name, `:` and hex digits.
    pub fn with_key_prefix(&self, format: &str, prefix: &str, limit: usize) -> Option<Vec<NodeId>> {
        let (alg, hex) = prefix.split_once(':')?;
        if !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return None;
        }
        let mut out = Vec::new();
        let Ok(k) = self.kinds.binary_search_by(|(f, a)| (f.as_str(), a.as_str()).cmp(&(format, alg))) else {
            return Some(out);
        };
        let lo: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| {
                let hi = u8::from_str_radix(&hex[i..i + 1], 16).unwrap();
                let lo = hex.get(i + 1..i + 2).map_or(0, |c| u8::from_str_radix(c, 16).unwrap());
                hi << 4 | lo
            })
            .collect();
        let starts = |i: usize| {
            let d = self.digest(i);
            hex.bytes().enumerate().all(|(j, c)| {
                let b = d.get(j / 2).copied().unwrap_or(0);
                let nib = if j % 2 == 0 { b >> 4 } else { b & 15 };
                b"0123456789abcdef"[usize::from(nib)] == c
            }) && hex.len() <= 2 * self.d
        };
        let (mut a, mut b) = (0, self.n);
        while a < b {
            let mid = (a + b) / 2;
            if self.sort_key(mid) < (k, &lo[..], 0) { a = mid + 1 } else { b = mid }
        }
        let mut i = a;
        while i < self.n && self.kind(i) == k && starts(i) && out.len() < limit {
            out.push(self.id(i));
            i += 1;
        }
        Some(out)
    }

    /// The id of node `i`.
    pub fn id(&self, i: usize) -> NodeId {
        let (f, a) = &self.kinds[self.kind(i)];
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut s = String::with_capacity(f.len() + a.len() + 2 + 2 * self.d + 11);
        s.push_str(f);
        s.push(' ');
        s.push_str(a);
        s.push(':');
        for b in self.digest(i) {
            s.push(HEX[usize::from(b >> 4)] as char);
            s.push(HEX[usize::from(b & 15)] as char);
        }
        let occ = self.occurrence(i);
        if occ != 0 {
            s.push('#');
            s.push_str(&occ.to_string());
        }
        // The kind's format is a blob key and its algorithm's digest is d
        // bytes (checked by decode); an occurrence is 0 or from 2.
        NodeId::trusted(s)
    }
}

/// The ancestry cached for the history at `input` itself, if any.
pub fn exact(store: &dyn Store, input: &View) -> Option<Ancestry> {
    let r = derived::readable_cache(store, &ANCESTRY, input).ok()??;
    Ancestry::decode(crate::read_blob(store, r.key()).ok()?).ok()
}

/// [`merge_bases`] from `index` alone: `None` when it lacks a node.
pub fn merge_bases_in(index: &Ancestry, a: &NodeId, b: &NodeId) -> Option<Vec<NodeId>> {
    let r = crate::htree::paint_down(a, b, |id| {
        let i = index.find(id).ok_or_else(|| Error::Missing(format!("node {id}")))?;
        Ok((u64::from(index.generation(i)), index.after(i).map(|p| index.id(p)).collect()))
    });
    r.ok().map(|s| s.into_iter().collect())
}

/// A cached ancestry usable for a history: of that history, or of one it
/// replaced, with the nodes that differ since.
pub struct Usable {
    pub index: Ancestry,
    /// Nodes the history holds otherwise than the index does (added or
    /// changed since): looked up in the history instead.
    pub stale: std::collections::HashSet<NodeId>,
}

/// The ancestry of the history `st` at `input`, or of a history it replaced
/// (within `limit` saves), if one is cached. Best effort.
pub fn cached(store: &dyn Store, st: &Stored, input: &View, limit: usize) -> Option<Usable> {
    let mut at = input.clone();
    for i in 0..=limit {
        if i > 0 {
            at = crate::graph::replaced(store, &at)?;
        }
        if let Ok(Some(r)) = derived::readable_cache(store, &ANCESTRY, &at) {
            let index = Ancestry::decode(crate::read_blob(store, r.key()).ok()?).ok()?;
            let stale = if i == 0 {
                Default::default()
            } else {
                Stored::open(store, Some(&at)).ok()?.changed(st).ok()?.added.into_keys().collect()
            };
            return Some(Usable { index, stale });
        }
    }
    None
}

/// Whether an ancestry is cached for the history at `input` or for one it
/// replaced within `limit` saves ([`cached`] would find it), without
/// reading it.
pub fn near(store: &dyn Store, input: &View, limit: usize) -> bool {
    let mut at = input.clone();
    for i in 0..=limit {
        if i > 0 {
            match crate::graph::replaced(store, &at) {
                Some(r) => at = r,
                None => return false,
            }
        }
        if matches!(derived::readable_cache(store, &ANCESTRY, &at), Ok(Some(_))) {
            return true;
        }
    }
    false
}

/// Record the ancestry of `h`, saved at `input`. Best effort.
pub fn record(store: &dyn Store, alg: &'static Algorithm, input: &View, h: &History) {
    if let Ok(b) = encode(h) {
        derived::record(store, alg, &ANCESTRY, input, &b);
    }
}

/// The merge bases of `a` and `b` in the history `st` ([`Stored::merge_bases`]),
/// taking nodes from `index` where it holds them and from `st` otherwise.
pub fn merge_bases(st: &Stored, index: Option<&Usable>, a: &NodeId, b: &NodeId) -> Result<Vec<NodeId>> {
    let look = |id: &NodeId| -> Result<(u64, Vec<NodeId>)> {
        if let Some(Usable { index: ix, stale }) = index
            && !stale.contains(id)
            && let Some(i) = ix.find(id)
        {
            return Ok((u64::from(ix.generation(i)), ix.after(i).map(|p| ix.id(p)).collect()));
        }
        let e = st.entry(id)?.ok_or_else(|| Error::Missing(format!("node {id}")))?;
        Ok((e.generation, e.node.after.into_keys().collect()))
    };
    Ok(crate::htree::paint_down(a, b, look)?.into_iter().collect())
}

/// For tests: the generations an index gives, by id.
pub fn generations(a: &Ancestry) -> BTreeMap<NodeId, u32> {
    (0..a.len()).map(|i| (a.id(i), a.generation(i))).collect()
}
