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

#[cfg(test)]
mod tests {
    use super::*;

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
