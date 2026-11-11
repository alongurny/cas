//! A hasher for sets and maps of blob keys (`alg:hex`): the last hex digits
//! of a digest are already uniform, so they are taken as the hash instead
//! of running SipHash over the whole key on every lookup. Other strings
//! fall back to FNV-1a, which is fine for the few variable names mixed in.

use std::hash::{BuildHasherDefault, Hasher};

#[derive(Default)]
pub struct KeyHasher(u64);

impl Hasher for KeyHasher {
    fn finish(&self) -> u64 {
        // A final mix, so the low bits hashbrown uses depend on all of it.
        let mut h = self.0;
        h ^= h >> 33;
        h = h.wrapping_mul(0xff51afd7ed558ccd);
        h ^ (h >> 33)
    }

    fn write(&mut self, b: &[u8]) {
        if b.len() >= 24 && b[b.len() - 16..].iter().all(u8::is_ascii_hexdigit) {
            let lo = u64::from_le_bytes(b[b.len() - 8..].try_into().unwrap());
            let hi = u64::from_le_bytes(b[b.len() - 16..b.len() - 8].try_into().unwrap());
            self.0 ^= lo.wrapping_mul(0x9e3779b97f4a7c15) ^ hi.rotate_left(29);
        } else {
            for &x in b {
                self.0 = (self.0 ^ u64::from(x)).wrapping_mul(0x100000001b3);
            }
        }
    }

    fn write_u8(&mut self, x: u8) {
        // `str` hashing ends with 0xff: nothing to add.
        if x != 0xff {
            self.write(&[x]);
        }
    }
}

pub type KeyState = BuildHasherDefault<KeyHasher>;
pub type KeySet = std::collections::HashSet<String, KeyState>;
pub type KeyMap<V> = std::collections::HashMap<String, V, KeyState>;

/// Hashes a digest by its first eight bytes, which are already uniform.
#[derive(Default)]
pub struct DigestHasher(u64);

impl Hasher for DigestHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, b: &[u8]) {
        for &x in b {
            self.0 = (self.0 ^ u64::from(x)).wrapping_mul(0x100000001b3);
        }
    }

    fn write_u64(&mut self, x: u64) {
        self.0 = x;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Digest([u8; 32]);

impl std::hash::Hash for Digest {
    fn hash<H: Hasher>(&self, h: &mut H) {
        h.write_u64(u64::from_le_bytes(self.0[..8].try_into().unwrap()));
    }
}

/// A set of blob keys that holds `blake3` keys as their 32 digest bytes,
/// so a walk over directory blobs can ask about an entry's digest without
/// spelling it out in hex. Other keys are kept as strings.
#[derive(Default)]
pub struct DigestSet {
    b3: std::collections::HashSet<Digest, BuildHasherDefault<DigestHasher>>,
    other: KeySet,
}

impl DigestSet {
    fn b3_of(k: &str) -> Option<Digest> {
        k.strip_prefix("blake3:").and_then(crate::pack::hex32).map(Digest)
    }

    /// Adds `k`; false if it was already there.
    pub fn insert(&mut self, k: &str) -> bool {
        match Self::b3_of(k) {
            Some(d) => self.b3.insert(d),
            None => !self.other.contains(k) && self.other.insert(k.to_string()),
        }
    }

    pub fn contains(&self, k: &str) -> bool {
        match Self::b3_of(k) {
            Some(d) => self.b3.contains(&d),
            None => self.other.contains(k),
        }
    }

    /// Whether the key `alg:hex(digest)` is in the set.
    pub fn contains_digest(&self, alg: &str, digest: &[u8]) -> bool {
        if alg == "blake3"
            && let Ok(d) = <[u8; 32]>::try_from(digest)
        {
            return self.b3.contains(&Digest(d));
        }
        let mut k = String::with_capacity(alg.len() + 1 + 2 * digest.len());
        k.push_str(alg);
        k.push(':');
        for b in digest {
            k.push_str(&format!("{b:02x}"));
        }
        self.other.contains(&k)
    }

    pub fn len(&self) -> usize {
        self.b3.len() + self.other.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl DigestSet {
    /// The set as bytes: `CASMARK1`, a count `n u32`, `n` blake3 digests
    /// sorted, then the other keys, each ended by a newline.
    pub fn encode(&self) -> Vec<u8> {
        let mut d: Vec<&[u8; 32]> = self.b3.iter().map(|d| &d.0).collect();
        d.sort_unstable();
        let mut out = Vec::with_capacity(12 + 32 * d.len());
        out.extend_from_slice(b"CASMARK1");
        out.extend_from_slice(&(d.len() as u32).to_be_bytes());
        for x in d {
            out.extend_from_slice(x);
        }
        let mut o: Vec<&String> = self.other.iter().collect();
        o.sort_unstable();
        for k in o {
            out.extend_from_slice(k.as_bytes());
            out.push(b'\n');
        }
        out
    }

    /// [`DigestSet::encode`] read back.
    pub fn decode(b: &[u8]) -> Option<DigestSet> {
        let rest = b.strip_prefix(b"CASMARK1")?;
        let n = u32::from_be_bytes(rest.get(..4)?.try_into().ok()?) as usize;
        let digests = rest.get(4..4 + 32 * n)?;
        let mut s = DigestSet::default();
        s.b3.reserve(n);
        for c in digests.as_chunks::<32>().0 {
            s.b3.insert(Digest(*c));
        }
        let tail = std::str::from_utf8(&rest[4 + 32 * n..]).ok()?;
        if !tail.is_empty() && !tail.ends_with('\n') {
            return None;
        }
        for k in tail.lines() {
            s.other.insert(k.to_string());
        }
        Some(s)
    }

    /// Moves every key of `other` into this set.
    pub fn absorb(&mut self, other: DigestSet) {
        self.b3.extend(other.b3);
        self.other.extend(other.other);
    }
}

/// A [`DigestSet`] that threads share, in shards so they seldom wait.
pub struct SharedDigestSet([std::sync::Mutex<DigestSet>; 64]);

impl Default for SharedDigestSet {
    fn default() -> Self {
        SharedDigestSet(std::array::from_fn(|_| Default::default()))
    }
}

impl SharedDigestSet {
    fn shard(&self, k: &str) -> &std::sync::Mutex<DigestSet> {
        let h = k.as_bytes().iter().rev().take(2).fold(0usize, |h, &c| h * 31 + usize::from(c));
        &self.0[h % 64]
    }

    /// Adds `k`; false if it was already there.
    pub fn insert(&self, k: &str) -> bool {
        self.shard(k).lock().unwrap().insert(k)
    }

    pub fn contains(&self, k: &str) -> bool {
        self.shard(k).lock().unwrap().contains(k)
    }

    pub fn into_set(self) -> DigestSet {
        let mut out = DigestSet::default();
        for m in self.0 {
            out.absorb(m.into_inner().unwrap());
        }
        out
    }
}

impl<S: AsRef<str>> FromIterator<S> for DigestSet {
    fn from_iter<I: IntoIterator<Item = S>>(it: I) -> Self {
        let mut s = DigestSet::default();
        for k in it {
            s.insert(k.as_ref());
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_sets_round_trip() {
        let keys = [format!("blake3:{}", "ab".repeat(32)), format!("blake3:{}", "01".repeat(32)), "sha256:00".into()];
        let s: DigestSet = keys.iter().collect();
        let t = DigestSet::decode(&s.encode()).unwrap();
        assert_eq!(t.len(), 3);
        assert!(keys.iter().all(|k| t.contains(k)));
        assert!(DigestSet::decode(&s.encode()[..20]).is_none());
    }

    #[test]
    fn digest_sets_answer_by_key_and_by_digest() {
        let alg = crate::algo::lookup("blake3").unwrap();
        let keys: Vec<String> = (0..1000u32).map(|i| alg.key(&i.to_le_bytes())).collect();
        let mut s: DigestSet = keys[..500].iter().collect();
        assert!(s.insert("sha256:00ff") && !s.insert("sha256:00ff") && !s.insert(&keys[0]));
        assert_eq!(s.len(), 501);
        for (i, k) in keys.iter().enumerate() {
            let digest = (0..32).map(|j| u8::from_str_radix(&k[7 + 2 * j..9 + 2 * j], 16).unwrap()).collect::<Vec<_>>();
            assert_eq!(s.contains(k), i < 500);
            assert_eq!(s.contains_digest("blake3", &digest), i < 500);
        }
        assert!(s.contains("sha256:00ff") && s.contains_digest("sha256", &[0, 255]));
        assert!(!s.contains("blake3:00") && !s.contains(&keys[0].to_uppercase()));
    }

    #[test]
    fn keys_spread_and_other_strings_still_hash() {
        let keys: Vec<String> =
            (0..10_000u32).map(|i| crate::algo::lookup("blake3").unwrap().key(&i.to_le_bytes())).collect();
        let s: KeySet = keys.iter().cloned().collect();
        assert_eq!(s.len(), keys.len());
        assert!(keys.iter().all(|k| s.contains(k)));
        let names: KeySet = ["main", "derived/x", "a", ""].iter().map(|x| x.to_string()).collect();
        assert!(names.contains("main") && !names.contains("b"));
        // Low bits differ across keys (hashbrown buckets by them).
        let lows: std::collections::HashSet<u64> = keys
            .iter()
            .map(|k| {
                let mut h = KeyHasher::default();
                std::hash::Hash::hash(k.as_str(), &mut h);
                h.finish() & 0xfff
            })
            .collect();
        assert!(lows.len() > 3000, "{} distinct low bit patterns", lows.len());
    }
}
