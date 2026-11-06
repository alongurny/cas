use std::fmt;

/// The state a CAS op expects the slot to be in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expect {
    Absent,
    Value(Vec<u8>),
}

impl Expect {
    pub fn matches(&self, current: Option<&[u8]>) -> bool {
        match (self, current) {
            (Expect::Absent, None) => true,
            (Expect::Value(v), Some(c)) => v.as_slice() == c,
            _ => false,
        }
    }
}

/// A half-open bytewise key range `[from, to)`. `None` is an open bound.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub from: Option<Vec<u8>>,
    pub to: Option<Vec<u8>>,
}

impl Span {
    /// Every key.
    pub fn all() -> Span {
        Span::default()
    }

    pub fn range(from: impl Into<Vec<u8>>, to: impl Into<Vec<u8>>) -> Span {
        Span { from: Some(from.into()), to: Some(to.into()) }
    }

    /// All keys starting with `p`, as the span `[p, p')`.
    pub fn prefix(p: impl AsRef<[u8]>) -> Span {
        let p = p.as_ref();
        if p.is_empty() {
            return Span::all();
        }
        Span { from: Some(p.to_vec()), to: prefix_successor(p) }
    }

    pub fn contains(&self, key: &[u8]) -> bool {
        self.from.as_deref().is_none_or(|f| key >= f) && self.to.as_deref().is_none_or(|t| key < t)
    }

    /// The rest of this span after `key` (exclusive).
    pub fn after(&self, key: &[u8]) -> Span {
        let mut from = key.to_vec();
        from.push(0);
        Span { from: Some(from), to: self.to.clone() }
    }
}

/// The smallest byte string greater than every string starting with `p`,
/// or `None` when no such string exists (p is all 0xff).
fn prefix_successor(p: &[u8]) -> Option<Vec<u8>> {
    let mut s = p.to_vec();
    while let Some(last) = s.pop() {
        if last < 0xff {
            s.push(last + 1);
            return Some(s);
        }
    }
    None
}

/// One answer to `list`: the first keys of the span, and the rest of the span
/// when the store answered only part of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListPage {
    pub keys: Vec<String>,
    pub rest: Option<Span>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invalid {
    /// Bad key syntax, or a digest of the wrong length for its algorithm.
    Key,
    /// A blob op whose `expect` is neither absent nor the blob's value.
    Expect,
    /// A blob value that does not hash to its key.
    HashMismatch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The gate refused. Carries nothing else, ever.
    Denied,
    Invalid(Invalid),
    /// Permitted and well formed, but this store's policy declines it.
    Refused {
        reason: String,
        message: Option<String>,
    },
    /// A variable's current state did not equal `expect`.
    Conflict,
    /// Transient failure. For write and remove the outcome is unknown.
    Unavailable(String),
}

impl Error {
    pub fn refused(reason: &str, message: impl Into<String>) -> Error {
        Error::Refused { reason: reason.to_string(), message: Some(message.into()) }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Denied => write!(f, "denied"),
            Error::Invalid(r) => write!(f, "invalid: {r:?}"),
            Error::Refused { reason, message: Some(m) } => write!(f, "refused: {reason}: {m}"),
            Error::Refused { reason, message: None } => write!(f, "refused: {reason}"),
            Error::Conflict => write!(f, "conflict"),
            Error::Unavailable(m) => write!(f, "unavailable: {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error::Unavailable(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
