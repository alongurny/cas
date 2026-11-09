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

// Batches (spec/wire.md, "Batches"). A frame is a u64 big-endian length
// and that many bytes.

pub fn put_frame(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    out.extend_from_slice(bytes);
}

/// The frame at the start of `*rest`, advancing past it.
pub fn take_frame<'a>(rest: &mut &'a [u8]) -> Option<&'a [u8]> {
    let (len, tail) = rest.split_at_checked(8)?;
    let len = usize::try_from(u64::from_be_bytes(len.try_into().ok()?)).ok()?;
    let (frame, tail) = tail.split_at_checked(len)?;
    *rest = tail;
    Some(frame)
}

/// Keys as consecutive frames.
pub fn encode_keys(keys: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    for k in keys {
        put_frame(&mut out, k.as_bytes());
    }
    out
}

pub fn decode_keys(mut body: &[u8]) -> Option<Vec<String>> {
    let mut keys = Vec::new();
    while !body.is_empty() {
        keys.push(String::from_utf8(take_frame(&mut body)?.to_vec()).ok()?);
    }
    Some(keys)
}

/// One outcome inside a batch answer: a kind letter, then its details.
pub fn encode_error(e: &Error) -> Vec<u8> {
    match e {
        Error::Denied => b"D".to_vec(),
        Error::Invalid(r) => format!("I{}", invalid_reason(*r)).into_bytes(),
        Error::Refused { reason, message } => {
            let mut out = b"R".to_vec();
            put_frame(&mut out, reason.as_bytes());
            if let Some(m) = message {
                out.extend_from_slice(m.as_bytes());
            }
            out
        }
        Error::Conflict => b"C".to_vec(),
        Error::Unavailable(m) => format!("U{m}").into_bytes(),
    }
}

pub fn decode_error(b: &[u8]) -> Option<Error> {
    let (kind, mut rest) = b.split_first()?;
    let text = |b: &[u8]| String::from_utf8(b.to_vec()).ok();
    Some(match kind {
        b'D' => Error::Denied,
        b'I' => Error::Invalid(parse_invalid_reason(std::str::from_utf8(rest).ok()?)?),
        b'R' => {
            let reason = text(take_frame(&mut rest)?)?;
            Error::Refused { reason, message: if rest.is_empty() { None } else { Some(text(rest)?) } }
        }
        b'C' => Error::Conflict,
        b'U' => Error::Unavailable(text(rest)?),
        _ => return None,
    })
}

/// A `batch/read` answer: for each key, `0` (absent), `1` and a frame
/// (the value), or `2` and a frame (an error).
pub fn encode_reads(results: &[Result<Option<Vec<u8>>, Error>]) -> Vec<u8> {
    let mut out = Vec::new();
    for r in results {
        match r {
            Ok(None) => out.push(0),
            Ok(Some(v)) => {
                out.push(1);
                put_frame(&mut out, v);
            }
            Err(e) => {
                out.push(2);
                put_frame(&mut out, &encode_error(e));
            }
        }
    }
    out
}

pub type ReadResult = Result<Option<Vec<u8>>, Error>;

pub fn decode_reads(mut body: &[u8]) -> Option<Vec<ReadResult>> {
    let mut out = Vec::new();
    while let Some((tag, rest)) = body.split_first() {
        body = rest;
        out.push(match tag {
            0 => Ok(None),
            1 => Ok(Some(take_frame(&mut body)?.to_vec())),
            2 => Err(decode_error(take_frame(&mut body)?)?),
            _ => return None,
        });
    }
    Some(out)
}

/// A `batch/stored` answer: as `batch/read`'s, and also `3`, a u64 length
/// and a frame (a zstd form), or `4`, a key frame (the base), a u64 length
/// and a frame (a delta form).
pub fn encode_stored(results: &[Result<Option<crate::store::Stored>, Error>]) -> Vec<u8> {
    use crate::store::{Form, Stored};
    let mut out = Vec::new();
    for r in results {
        match r {
            Ok(None) => out.push(0),
            Ok(Some(Stored::Value(v))) => {
                out.push(1);
                put_frame(&mut out, v);
            }
            Ok(Some(Stored::Form { len, form: Form::Zstd(z) })) => {
                out.push(3);
                out.extend_from_slice(&(*len as u64).to_be_bytes());
                put_frame(&mut out, z);
            }
            Ok(Some(Stored::Form { len, form: Form::Delta { base, z } })) => {
                out.push(4);
                put_frame(&mut out, base.as_bytes());
                out.extend_from_slice(&(*len as u64).to_be_bytes());
                put_frame(&mut out, z);
            }
            Err(e) => {
                out.push(2);
                put_frame(&mut out, &encode_error(e));
            }
        }
    }
    out
}

pub type StoredResult = Result<Option<crate::store::Stored>, Error>;

pub fn decode_stored(mut body: &[u8]) -> Option<Vec<StoredResult>> {
    use crate::store::{Form, Stored};
    let take_len = |b: &mut &[u8]| -> Option<usize> {
        let (n, rest) = b.split_at_checked(8)?;
        *b = rest;
        usize::try_from(u64::from_be_bytes(n.try_into().ok()?)).ok()
    };
    let mut out = Vec::new();
    while let Some((tag, rest)) = body.split_first() {
        body = rest;
        out.push(match tag {
            0 => Ok(None),
            1 => Ok(Some(Stored::Value(take_frame(&mut body)?.to_vec()))),
            2 => Err(decode_error(take_frame(&mut body)?)?),
            3 => {
                let len = take_len(&mut body)?;
                Ok(Some(Stored::Form { len, form: Form::Zstd(take_frame(&mut body)?.to_vec()) }))
            }
            4 => {
                let base = String::from_utf8(take_frame(&mut body)?.to_vec()).ok()?;
                let len = take_len(&mut body)?;
                Ok(Some(Stored::Form { len, form: Form::Delta { base, z: take_frame(&mut body)?.to_vec() } }))
            }
            _ => return None,
        });
    }
    Some(out)
}

/// A `batch/write` body: for each write, a key frame, an expect frame and
/// a value frame.
pub fn encode_puts(puts: &[crate::store::Put]) -> Vec<u8> {
    let mut out = Vec::new();
    for p in puts {
        put_frame(&mut out, p.key.as_bytes());
        encode_expect(&p.expect, &mut out);
        put_frame(&mut out, &p.value);
    }
    out
}

pub fn decode_puts(mut body: &[u8]) -> Option<Vec<crate::store::Put>> {
    let mut out = Vec::new();
    while !body.is_empty() {
        let key = String::from_utf8(take_frame(&mut body)?.to_vec()).ok()?;
        let (expect, rest) = decode_expect(body)?;
        body = rest;
        let value = take_frame(&mut body)?.to_vec();
        out.push(crate::store::Put { key, value, expect, form: None });
    }
    Some(out)
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
