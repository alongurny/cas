//! What a storage backend must provide. Backends see only validated input and
//! never interpret values; [`crate::CoreStore`] does all checking.

use crate::types::{Expect, Span};
use std::io;

pub trait Backend: Send + Sync {
    fn get(&self, key: &str) -> io::Result<Option<Vec<u8>>>;

    /// Store a verified blob. Idempotent; durable on return.
    fn put_blob(&self, key: &str, value: &[u8]) -> io::Result<()>;

    /// [`Backend::put_blob`] of each, in order: on return all are durable,
    /// and none is visible before the ones it follows. Backends override
    /// this to make the batch durable at once.
    fn put_blobs(&self, blobs: &[(&str, &[u8])]) -> io::Result<()> {
        blobs.iter().try_for_each(|(k, v)| self.put_blob(k, v))
    }

    /// Idempotent; durable on return.
    fn remove_blob(&self, key: &str) -> io::Result<()>;

    /// Atomically: if the variable's state equals `expect`, set it to `new`
    /// (`None` removes it) and return true; else return false and change
    /// nothing. Linearizable per variable; durable on return.
    fn cas_var(&self, name: &str, expect: &Expect, new: Option<&[u8]>) -> io::Result<bool>;

    /// Up to `limit` keys in `span`, ascending bytewise.
    fn list(&self, span: &Span, limit: usize) -> io::Result<Vec<String>>;

    /// Whether this backend keeps chunks (spec/wire.md, chunked transfer).
    /// The chunk methods below are used only when it does.
    fn chunked(&self) -> bool {
        false
    }

    /// The bytes of chunk `id`, if held.
    fn get_chunk(&self, _id: &crate::chunk::Id) -> io::Result<Option<Vec<u8>>> {
        Ok(None)
    }

    /// Store a verified chunk. Idempotent; durable on return.
    fn put_chunk(&self, _id: &crate::chunk::Id, _bytes: &[u8]) -> io::Result<()> {
        Err(io::Error::other("this backend keeps no chunks"))
    }
}
