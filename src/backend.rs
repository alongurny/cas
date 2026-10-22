//! What a storage backend must provide. Backends see only validated input and
//! never interpret values; [`crate::CoreStore`] does all checking.

use crate::types::{Expect, Span};
use std::io;

pub trait Backend: Send + Sync {
    fn get(&self, key: &str) -> io::Result<Option<Vec<u8>>>;

    /// Store a verified blob. Idempotent; durable on return.
    fn put_blob(&self, key: &str, value: &[u8]) -> io::Result<()>;

    /// Idempotent; durable on return.
    fn remove_blob(&self, key: &str) -> io::Result<()>;

    /// Atomically: if the tag's state equals `expect`, set it to `new`
    /// (`None` removes it) and return true; else return false and change
    /// nothing. Linearizable per tag; durable on return.
    fn cas_tag(&self, name: &str, expect: &Expect, new: Option<&[u8]>) -> io::Result<bool>;

    /// Up to `limit` keys in `span`, ascending bytewise.
    fn list(&self, span: &Span, limit: usize) -> io::Result<Vec<String>>;
}
