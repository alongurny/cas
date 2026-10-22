//! The hash algorithm registry: a fixed map from names to functions. Which of
//! these a store accepts is store policy; the core has no default.

use sha2::Digest;

pub struct Algorithm {
    pub name: &'static str,
    /// Digest length in bytes.
    pub digest_len: usize,
    /// Registered so the name keeps one meaning, but no longer collision
    /// resistant. Whether a store accepts it is store policy.
    pub broken: bool,
    hash: fn(&[u8]) -> Vec<u8>,
}

impl Algorithm {
    pub fn hash(&self, data: &[u8]) -> Vec<u8> {
        (self.hash)(data)
    }

    /// The full blob key of `data` under this algorithm.
    pub fn key(&self, data: &[u8]) -> String {
        format!("{}:{}", self.name, hex::encode(self.hash(data)))
    }
}

pub static REGISTRY: &[Algorithm] = &[
    Algorithm { name: "blake3", digest_len: 32, broken: false, hash: |d| blake3::hash(d).as_bytes().to_vec() },
    Algorithm { name: "sha256", digest_len: 32, broken: false, hash: |d| sha2::Sha256::digest(d).to_vec() },
    Algorithm { name: "sha512", digest_len: 64, broken: false, hash: |d| sha2::Sha512::digest(d).to_vec() },
    Algorithm { name: "sha3-256", digest_len: 32, broken: false, hash: |d| sha3::Sha3_256::digest(d).to_vec() },
    Algorithm { name: "sha1", digest_len: 20, broken: true, hash: |d| sha1::Sha1::digest(d).to_vec() },
    Algorithm { name: "md5", digest_len: 16, broken: true, hash: |d| md5::Md5::digest(d).to_vec() },
];

pub fn lookup(name: &str) -> Option<&'static Algorithm> {
    REGISTRY.iter().find(|a| a.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_test_vectors() {
        let b3 = lookup("blake3").unwrap();
        let s256 = lookup("sha256").unwrap();
        assert_eq!(b3.key(b""), "blake3:af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262");
        assert_eq!(b3.key(b"hello\n"), "blake3:8e4c7c1b99dbfd50e7a95185fead5ee1448fa904a2fdd778eaf5f2dbfd629a99");
        assert_eq!(s256.key(b""), "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(s256.key(b"hello\n"), "sha256:5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03");
    }

    #[test]
    fn digest_lengths_match() {
        for a in REGISTRY {
            assert_eq!(a.hash(b"x").len(), a.digest_len, "{}", a.name);
        }
    }
}
