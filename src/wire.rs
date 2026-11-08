//! Encodings shared by the HTTP server and client (spec/wire.md).

use crate::types::{Error, Expect, Invalid};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};

/// Everything but `A-Z a-z 0-9 - . _ ~`.
const KEY_SET: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');

/// The Content-Type of a blob write whose value is a list of chunk ids.
pub const WRITE_CHUNKS: &str = "application/vnd.cas.write-chunks";

pub fn encode_key(key: &str) -> String {
    utf8_percent_encode(key, KEY_SET).to_string()
}

pub fn decode_key(segment: &str) -> Result<String, Error> {
    percent_decode_str(segment).decode_utf8().map(|k| k.into_owned()).map_err(|_| Error::Invalid(Invalid::Key))
}

pub fn encode_expect(expect: &Expect, out: &mut Vec<u8>) {
    match expect {
        Expect::Absent => out.push(0),
        Expect::Value(v) => {
            out.push(1);
            out.extend_from_slice(&(v.len() as u64).to_be_bytes());
            out.extend_from_slice(v);
        }
    }
}

/// The expect frame at the start of `body`, and the bytes after it.
pub fn decode_expect(body: &[u8]) -> Option<(Expect, &[u8])> {
    match body.split_first()? {
        (0, rest) => Some((Expect::Absent, rest)),
        (1, rest) => {
            let (len, rest) = rest.split_at_checked(8)?;
            let len = usize::try_from(u64::from_be_bytes(len.try_into().ok()?)).ok()?;
            let (value, rest) = rest.split_at_checked(len)?;
            Some((Expect::Value(value.to_vec()), rest))
        }
        _ => None,
    }
}

pub fn invalid_reason(r: Invalid) -> &'static str {
    match r {
        Invalid::Key => "key",
        Invalid::Expect => "expect",
        Invalid::HashMismatch => "hash-mismatch",
        Invalid::View => "view",
    }
}

pub fn parse_invalid_reason(s: &str) -> Option<Invalid> {
    match s {
        "key" => Some(Invalid::Key),
        "expect" => Some(Invalid::Expect),
        "hash-mismatch" => Some(Invalid::HashMismatch),
        "view" => Some(Invalid::View),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip() {
        for k in ["main", "refs/heads/main", "blake3:ab", "dérivé/x y?#%"] {
            let e = encode_key(k);
            assert!(!e.contains('/') && !e.contains('?') && !e.contains('#'));
            assert_eq!(decode_key(&e).unwrap(), k);
        }
    }

    #[test]
    fn expect_frames_round_trip() {
        for e in [Expect::Absent, Expect::Value(vec![]), Expect::Value(b"abc".to_vec())] {
            let mut buf = Vec::new();
            encode_expect(&e, &mut buf);
            buf.extend_from_slice(b"tail");
            assert_eq!(decode_expect(&buf), Some((e, &b"tail"[..])));
        }
        assert_eq!(decode_expect(&[1, 0, 0, 0, 0, 0, 0, 0, 9, 1]), None);
        assert_eq!(decode_expect(&[2]), None);
        assert_eq!(decode_expect(&[]), None);
    }
}
