//! Key syntax (spec section 2). One namespace; a key's kind follows from its
//! syntax alone.

/// A syntactically valid key, classified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Parsed<'a> {
    Blob { alg: &'a str, digest_hex: &'a str },
    Var,
}

/// Classify `key`, or `None` when it breaks the key byte rules or the blob
/// grammar. Digest length is checked later, once the algorithm is known.
const ALG: u8 = 1;
const HEX: u8 = 2;
/// Byte classes: ALG for `[a-z0-9-]`, HEX for `[0-9a-f]`.
static CLASS: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut c = 0;
    while c < 256 {
        let b = c as u8;
        if b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' {
            t[c] |= ALG;
        }
        if b.is_ascii_digit() || (b >= b'a' && b <= b'f') {
            t[c] |= HEX;
        }
        c += 1;
    }
    t
};

pub fn classify(key: &str) -> Option<Parsed<'_>> {
    // Fast path, one pass: `alg:hex` with a valid algorithm name, which is
    // every blob key. Such a key has no control byte and no `/` before its
    // first colon, so the full rules below would agree.
    let b = key.as_bytes();
    let alg_len = b.iter().take(33).take_while(|&&c| CLASS[usize::from(c)] & ALG != 0).count();
    if (1..=32).contains(&alg_len) && b.get(alg_len) == Some(&b':') {
        let d = &b[alg_len + 1..];
        // A table and no early exit: keys are nearly always valid.
        if !d.is_empty() && d.iter().fold(HEX, |m, &c| m & CLASS[usize::from(c)]) != 0 {
            return Some(Parsed::Blob { alg: &key[..alg_len], digest_hex: &key[alg_len + 1..] });
        }
    }
    // Control characters: bytes below 0x20 and 0x7f in ASCII, and U+0080
    // to U+009F otherwise. The byte scan is the common, all-ASCII case.
    let controls =
        if key.is_ascii() { key.bytes().any(|b| b < 0x20 || b == 0x7f) } else { key.chars().any(|c| c.is_control()) };
    if key.is_empty() || controls {
        return None;
    }
    let Some(colon) = key.find(':') else { return Some(Parsed::Var) };
    if key[..colon].contains('/') {
        return Some(Parsed::Var);
    }
    let (alg, digest_hex) = (&key[..colon], &key[colon + 1..]);
    let alg_ok =
        (1..=32).contains(&alg.len()) && alg.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    let digest_ok = !digest_hex.is_empty() && digest_hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    (alg_ok && digest_ok).then_some(Parsed::Blob { alg, digest_hex })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        assert_eq!(classify("main"), Some(Parsed::Var));
        assert_eq!(classify("refs/heads/main"), Some(Parsed::Var));
        assert_eq!(classify("derived/blame/blake3:8e4c"), Some(Parsed::Var));
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
