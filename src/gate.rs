//! The permission gate (spec section 8). It sees only who is asking, the op
//! and its arguments, never store contents, and it runs before anything else.

use crate::types::{Expect, Span};

/// The caller, as established by the outer protocol. The core carries no
/// credentials; `None` is an unauthenticated caller.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Who(pub Option<String>);

impl Who {
    pub fn anonymous() -> Who {
        Who(None)
    }

    pub fn named(name: impl Into<String>) -> Who {
        Who(Some(name.into()))
    }
}

/// An op and its full arguments, exactly as the caller sent them.
#[derive(Clone, Copy, Debug)]
pub enum Op<'a> {
    Read { key: &'a str },
    Write { key: &'a str, value: &'a [u8], expect: &'a Expect },
    Remove { key: &'a str, expect: &'a Expect },
    List { span: &'a Span },
}

pub trait Gate: Send + Sync {
    fn allow(&self, who: &Who, op: &Op<'_>) -> bool;

    /// Whether a `has` of `key` (an allowed one-key list) answers "absent"
    /// to `who` whether or not the store holds it: a gate that scopes blob
    /// reads hides the blobs a caller may not read, so asking reveals
    /// nothing a read would not.
    fn hides(&self, _who: &Who, _key: &str) -> bool {
        false
    }
}

/// The Stage 2 default.
pub struct AllowAll;

impl Gate for AllowAll {
    fn allow(&self, _: &Who, _: &Op<'_>) -> bool {
        true
    }
}

pub struct DenyAll;

impl Gate for DenyAll {
    fn allow(&self, _: &Who, _: &Op<'_>) -> bool {
        false
    }
}

impl<F> Gate for F
where
    F: Fn(&Who, &Op<'_>) -> bool + Send + Sync,
{
    fn allow(&self, who: &Who, op: &Op<'_>) -> bool {
        self(who, op)
    }
}
