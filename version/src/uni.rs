//! The `uni-bin` format (spec/formats/uni-bin.md): strings and maps. Blobs hold the
//! binary serialization; the text notation is for people.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;

/// A string, or a map from values to values.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Value {
    Str(String),
    Map(BTreeMap<Value, Value>),
}

/// Bytewise order of serializations, which is the order of map entries.
/// Computed without serializing: a serialization is a tag, a LEB128
/// length and the content, and serializations are prefix-free, so
/// comparing two maps entry by entry gives the order of their bytes.
impl Ord for Value {
    fn cmp(&self, other: &Value) -> Ordering {
        let len = |n: usize| {
            let mut b = [0u8; 10];
            let mut v = Vec::new();
            leb128(&mut v, n);
            b[..v.len()].copy_from_slice(&v);
            (b, v.len())
        };
        let lens = |a: usize, b: usize| {
            if a == b {
                return Ordering::Equal;
            }
            let ((x, i), (y, j)) = (len(a), len(b));
            x[..i].cmp(&y[..j])
        };
        match (self, other) {
            (Value::Str(_), Value::Map(_)) => Ordering::Less,
            (Value::Map(_), Value::Str(_)) => Ordering::Greater,
            (Value::Str(a), Value::Str(b)) => lens(a.len(), b.len()).then_with(|| a.as_bytes().cmp(b.as_bytes())),
            (Value::Map(a), Value::Map(b)) => lens(a.len(), b.len()).then_with(|| {
                for ((ka, va), (kb, vb)) in a.iter().zip(b) {
                    let o = ka.cmp(kb).then_with(|| va.cmp(vb));
                    if o != Ordering::Equal {
                        return o;
                    }
                }
                Ordering::Equal
            }),
        }
    }
}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Value) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Value {
    pub fn str(s: impl Into<String>) -> Value {
        Value::Str(s.into())
    }

    pub fn empty() -> Value {
        Value::Map(BTreeMap::new())
    }

    pub fn map<K: Into<Value>, V: Into<Value>>(entries: impl IntoIterator<Item = (K, V)>) -> Value {
        Value::Map(entries.into_iter().map(|(k, v)| (k.into(), v.into())).collect())
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            Value::Map(_) => None,
        }
    }

    pub fn as_map(&self) -> Option<&BTreeMap<Value, Value>> {
        match self {
            Value::Map(m) => Some(m),
            Value::Str(_) => None,
        }
    }

    /// The serialization: what is stored and hashed.
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.ser(&mut out);
        out
    }

    fn ser(&self, out: &mut Vec<u8>) {
        match self {
            Value::Str(s) => {
                out.push(0);
                leb128(out, s.len());
                out.extend_from_slice(s.as_bytes());
            }
            Value::Map(m) => {
                out.push(1);
                leb128(out, m.len());
                for (k, v) in m {
                    k.ser(out);
                    v.ser(out);
                }
            }
        }
    }

    /// Parse a serialization; anything that is not exactly one is an error.
    pub fn deserialize(bytes: &[u8]) -> Result<Value, String> {
        let mut at = 0;
        let v = de(bytes, &mut at, 0)?;
        if at != bytes.len() {
            return Err(format!("trailing bytes at {at}"));
        }
        Ok(v)
    }

    /// The text notation.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            Value::Str(s) => {
                out.push('\'');
                for c in s.chars() {
                    if c == '\\' || c == '\'' {
                        out.push('\\');
                    }
                    out.push(c);
                }
                out.push('\'');
            }
            Value::Map(m) => {
                out.push('{');
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    k.write(out);
                    out.push_str(": ");
                    v.write(out);
                }
                out.push('}');
            }
        }
    }

    /// Parse the text notation, in its one canonical spelling.
    pub fn from_text(text: &str) -> Result<Value, String> {
        let mut p = Parser { s: text, at: 0, depth: 0 };
        let v = p.value()?;
        if p.at != text.len() {
            return Err(format!("trailing bytes at {}", p.at));
        }
        // Parsing accepts entries in any order; the notation has them sorted.
        if v.to_text() != text {
            return Err("not in canonical form".into());
        }
        Ok(v)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Value {
        Value::str(s)
    }
}

impl From<String> for Value {
    fn from(s: String) -> Value {
        Value::Str(s)
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_text())
    }
}

fn leb128(out: &mut Vec<u8>, mut n: usize) {
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn read_leb128(b: &[u8], at: &mut usize) -> Result<usize, String> {
    let (mut n, mut shift) = (0usize, 0u32);
    loop {
        let byte = *b.get(*at).ok_or("truncated length")?;
        *at += 1;
        let part = ((byte & 0x7f) as usize).checked_shl(shift).filter(|p| p >> shift == (byte & 0x7f) as usize);
        n |= part.ok_or("length overflows")?;
        if byte & 0x80 == 0 {
            // Shortest form: no final zero byte after the first.
            if byte == 0 && shift > 0 {
                return Err("length not in shortest form".into());
            }
            return Ok(n);
        }
        shift += 7;
    }
}

/// More maps nested than this are refused: parsing, comparing, serializing
/// and dropping a value all recurse, so an untrusted blob of a few hundred
/// kilobytes of `01 01 …` would otherwise overflow the stack and abort.
pub const MAX_DEPTH: usize = 256;

fn de(b: &[u8], at: &mut usize, depth: usize) -> Result<Value, String> {
    let tag = *b.get(*at).ok_or("truncated value")?;
    *at += 1;
    let len = read_leb128(b, at)?;
    match tag {
        0 => {
            let end = at.checked_add(len).filter(|&e| e <= b.len()).ok_or("truncated string")?;
            let s = std::str::from_utf8(&b[*at..end]).map_err(|_| "string is not UTF-8")?;
            *at = end;
            Ok(Value::Str(s.to_string()))
        }
        1 => {
            if depth >= MAX_DEPTH {
                return Err(format!("maps nested deeper than {MAX_DEPTH} at {}", *at - 1));
            }
            let mut m = Vec::with_capacity(len.min(b.len()));
            let mut prev: Option<&[u8]> = None;
            for _ in 0..len {
                let start = *at;
                let k = de(b, at, depth + 1)?;
                let kb = &b[start..*at];
                if prev.is_some_and(|p| p >= kb) {
                    return Err(format!("map key at {start} out of order"));
                }
                prev = Some(kb);
                let v = de(b, at, depth + 1)?;
                m.push((k, v));
            }
            // Already in order: the bulk build makes no reordering.
            Ok(Value::Map(m.into_iter().collect()))
        }
        t => Err(format!("unknown tag {t}")),
    }
}

struct Parser<'a> {
    s: &'a str,
    at: usize,
    depth: usize,
}

impl Parser<'_> {
    fn eat(&mut self, lit: &str) -> bool {
        if self.s[self.at..].starts_with(lit) {
            self.at += lit.len();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, lit: &str) -> Result<(), String> {
        if self.eat(lit) { Ok(()) } else { Err(format!("expected {lit:?} at {}", self.at)) }
    }

    fn value(&mut self) -> Result<Value, String> {
        if self.eat("'") {
            let mut s = String::new();
            let mut chars = self.s[self.at..].char_indices();
            loop {
                let Some((i, c)) = chars.next() else { return Err("unterminated string".into()) };
                match c {
                    '\'' => {
                        self.at += i + 1;
                        return Ok(Value::Str(s));
                    }
                    '\\' => match chars.next() {
                        Some((_, e @ ('\\' | '\''))) => s.push(e),
                        _ => return Err(format!("bad escape at {}", self.at + i)),
                    },
                    c => s.push(c),
                }
            }
        }
        self.expect("{")?;
        if self.depth >= MAX_DEPTH {
            return Err(format!("maps nested deeper than {MAX_DEPTH} at {}", self.at));
        }
        self.depth += 1;
        let v = self.map();
        self.depth -= 1;
        v
    }

    fn map(&mut self) -> Result<Value, String> {
        let mut m = BTreeMap::new();
        if self.eat("}") {
            return Ok(Value::Map(m));
        }
        loop {
            let k = self.value()?;
            self.expect(": ")?;
            let v = self.value()?;
            if m.insert(k, v).is_some() {
                return Err(format!("duplicate key before {}", self.at));
            }
            if self.eat("}") {
                return Ok(Value::Map(m));
            }
            self.expect(", ")?;
        }
    }
}

/// A reader over a serialization that checks the same rules as
/// [`Value::deserialize`] (tags, shortest lengths, strictly ascending map
/// keys, UTF-8, depth) without building values: for formats that map a
/// serialization straight into their own types.
pub(crate) struct Cursor<'a> {
    pub b: &'a [u8],
    pub at: usize,
    depth: usize,
}

/// One map being read: yields its keys in order, checking the order.
pub(crate) struct MapReader {
    left: usize,
    prev: Option<(usize, usize)>,
}

impl<'a> Cursor<'a> {
    pub fn new(b: &'a [u8]) -> Cursor<'a> {
        Cursor { b, at: 0, depth: 0 }
    }

    fn head(&mut self) -> Result<(u8, usize), String> {
        let tag = *self.b.get(self.at).ok_or("truncated value")?;
        self.at += 1;
        let len = read_leb128(self.b, &mut self.at)?;
        Ok((tag, len))
    }

    pub fn peek_tag(&self) -> Option<u8> {
        self.b.get(self.at).copied()
    }

    /// A string value.
    pub fn str(&mut self) -> Result<&'a str, String> {
        match self.head()? {
            (0, len) => {
                let end = self.at.checked_add(len).filter(|&e| e <= self.b.len()).ok_or("truncated string")?;
                let s = std::str::from_utf8(&self.b[self.at..end]).map_err(|_| "string is not UTF-8")?;
                self.at = end;
                Ok(s)
            }
            (1, _) => Err("a map where a string belongs".into()),
            (t, _) => Err(format!("unknown tag {t}")),
        }
    }

    /// Start a map value; read its entries with [`Cursor::key`] and then
    /// each value, and finish with [`Cursor::end`].
    pub fn map(&mut self) -> Result<MapReader, String> {
        let start = self.at;
        match self.head()? {
            (1, len) => {
                if self.depth >= MAX_DEPTH {
                    return Err(format!("maps nested deeper than {MAX_DEPTH} at {start}"));
                }
                self.depth += 1;
                Ok(MapReader { left: len, prev: None })
            }
            (0, _) => {
                self.at = start;
                Err("a string where a map belongs".into())
            }
            (t, _) => Err(format!("unknown tag {t}")),
        }
    }

    /// The next key of `m` if it is a string (`Ok(None)` when the map is
    /// done). A map key is an error here: the formats using this have
    /// string keys only.
    pub fn key(&mut self, m: &mut MapReader, what: &str) -> Result<Option<&'a str>, String> {
        if m.left == 0 {
            self.depth -= 1;
            return Ok(None);
        }
        m.left -= 1;
        let start = self.at;
        if self.peek_tag() == Some(1) {
            // Still check it is well formed, as deserialize would first.
            de(self.b, &mut self.at, self.depth)?;
            return Err(format!("{what} has a map key"));
        }
        let k = self.str()?;
        let kb = &self.b[start..self.at];
        if m.prev.is_some_and(|(s, e)| &self.b[s..e] >= kb) {
            return Err(format!("map key at {start} out of order"));
        }
        m.prev = Some((start, self.at));
        Ok(Some(k))
    }

    pub fn done(&self) -> Result<(), String> {
        if self.at == self.b.len() { Ok(()) } else { Err(format!("trailing bytes at {}", self.at)) }
    }
}
