//! Key syntax (spec section 2). One namespace; a key's kind follows from its
//! syntax alone.

/// A syntactically valid key, classified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Parsed<'a> {
    Blob { alg: &'a str, digest_hex: &'a str },
    Tag,
}

/// Classify `key`, or `None` when it breaks the key byte rules or the blob
/// grammar. Digest length is checked later, once the algorithm is known.
pub fn classify(key: &str) -> Option<Parsed<'_>> {
    if key.is_empty() || key.chars().any(|c| c.is_control()) {
        return None;
    }
    let Some(colon) = key.find(':') else { return Some(Parsed::Tag) };
    if key[..colon].contains('/') {
        return Some(Parsed::Tag);
    }
    let (alg, digest_hex) = (&key[..colon], &key[colon + 1..]);
    let alg_ok = (1..=32).contains(&alg.len())
        && alg.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    let digest_ok = !digest_hex.is_empty() && digest_hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    (alg_ok && digest_ok).then_some(Parsed::Blob { alg, digest_hex })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        assert_eq!(classify("main"), Some(Parsed::Tag));
        assert_eq!(classify("refs/heads/main"), Some(Parsed::Tag));
        assert_eq!(classify("derived/blame/blake3:8e4c"), Some(Parsed::Tag));
        assert_eq!(classify("blake3:af13"), Some(Parsed::Blob { alg: "blake3", digest_hex: "af13" }));
        assert_eq!(classify("foo:bar"), None); // not hex
        assert_eq!(classify("http://x"), None);
        assert_eq!(classify("blake3:AF13"), None);
        assert_eq!(classify("Blake3:af13"), None);
        assert_eq!(classify(":af13"), None);
        assert_eq!(classify(""), None);
        assert_eq!(classify("a\nb"), None);
        assert_eq!(classify("a\u{7f}"), None);
    }
}
