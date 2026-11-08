//! Content-defined chunking of large blobs (spec/wire.md, "Chunked
//! transfer"). Fixed parameters: both ends of a transfer and the disk
//! backend must cut the same bytes at the same places.

/// Blobs larger than this are stored and sent as chunks.
pub const CHUNK_OVER: usize = 64 * 1024;
const MIN: u32 = 4 * 1024;
const AVG: u32 = 16 * 1024;
const MAX: u32 = 64 * 1024;

/// A chunk id: the blake3 hash of its bytes.
pub type Id = [u8; 32];

/// The chunks of `value`, in order, with their ids.
pub fn split(value: &[u8]) -> Vec<(Id, &[u8])> {
    fastcdc::v2020::FastCDC::new(value, MIN, AVG, MAX)
        .map(|c| {
            let piece = &value[c.offset..c.offset + c.length];
            (*blake3::hash(piece).as_bytes(), piece)
        })
        .collect()
}

pub fn id_of(bytes: &[u8]) -> Id {
    *blake3::hash(bytes).as_bytes()
}
