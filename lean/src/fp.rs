//! Fingerprints (spec/lean/tables.md, "Ids and fingerprints"): a row's id is
//! the polynomial with the row's fields as coefficients, evaluated at a
//! random point. Natively the point is four independent elements of the
//! Goldilocks field, which gives the same collision bound as one point of
//! its degree-4 extension.

/// The Goldilocks prime, 2^64 - 2^32 + 1.
pub const P: u64 = 0xffff_ffff_0000_0001;

fn add(a: u64, b: u64) -> u64 {
    let (s, c) = a.overflowing_add(b);
    let s = if c { s.wrapping_add(0xffff_ffff) } else { s };
    if s >= P { s - P } else { s }
}

fn mul(a: u64, b: u64) -> u64 {
    ((a as u128 * b as u128) % P as u128) as u64
}

/// A row's id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Id(pub [u64; 4]);

/// The random point: drawn after the tables are fixed.
#[derive(Clone, Copy, Debug)]
pub struct Point([u64; 4]);

impl Point {
    /// The point drawn from a commitment to the tables (Fiat-Shamir).
    pub fn from_commitment(commitment: &[u8; 32]) -> Point {
        let mut h = blake3::Hasher::new_derive_key("cas-lean fingerprint point");
        h.update(commitment);
        let mut out = [0u8; 32];
        h.finalize_xof().fill(&mut out);
        let lane = |i: usize| u64::from_le_bytes(out[8 * i..8 * i + 8].try_into().unwrap()) % P;
        Point([lane(0), lane(1), lane(2), lane(3)])
    }

    /// The id of a row with these fields, the first being its kind.
    pub fn id(&self, fields: &[Field]) -> Id {
        let mut acc = [0u64; 4];
        // Horner, from the last field: x0 + r (x1 + r (x2 + ...)).
        for f in fields.iter().rev() {
            for (lane, a) in acc.iter_mut().enumerate() {
                let x = match f {
                    Field::Num(n) => n % P,
                    Field::Id(id) => id.0[lane],
                };
                *a = add(mul(*a, self.0[lane]), x);
            }
        }
        Id(acc)
    }
}

/// One coefficient: a number below 2^32 (so that no two differ by a
/// multiple of P), or a child's id.
#[derive(Clone, Copy, Debug)]
pub enum Field {
    Num(u64),
    Id(Id),
}

/// Bytes as fields: their count, then 32-bit limbs.
pub fn limbs(bytes: &[u8], out: &mut Vec<Field>) {
    out.push(Field::Num(bytes.len() as u64));
    for c in bytes.chunks(4) {
        let mut w = [0u8; 4];
        w[..c.len()].copy_from_slice(c);
        out.push(Field::Num(u32::from_le_bytes(w) as u64));
    }
}
