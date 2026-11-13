//! The arithmetic Lean's kernel evaluates on natural number literals
//! (type_checker.cpp `reduce_nat`), with the same results: truncated
//! subtraction, division and modulus by zero give 0 and the dividend, and
//! powers only for exponents up to 2^24.

use crate::derivation::Prim;
use num_bigint::BigUint;
use num_integer::Integer;

/// The value of a literal's bytes (little-endian).
pub fn value(bytes: &[u8]) -> BigUint {
    BigUint::from_bytes_le(bytes)
}

/// The bytes of a literal: little-endian, without trailing zeros.
pub fn bytes(n: &BigUint) -> Vec<u8> {
    if *n == BigUint::ZERO { vec![] } else { n.to_bytes_le() }
}

/// What an operation gives: a number or a boolean.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Nat(BigUint),
    Bool(bool),
}

const MAX_POW_EXP: u32 = 1 << 24;

/// The operation `op` on `a` and `b`, when the kernel evaluates it.
pub fn eval(op: Prim, a: &BigUint, b: &BigUint) -> Option<Value> {
    use Prim::*;
    let n = |x: BigUint| Some(Value::Nat(x));
    let small = |x: &BigUint| u64::try_from(x).ok();
    match op {
        NatAdd => n(a + b),
        NatSub => n(if a >= b { a - b } else { BigUint::ZERO }),
        NatMul => n(a * b),
        NatDiv => n(if *b == BigUint::ZERO { BigUint::ZERO } else { a / b }),
        NatMod => n(if *b == BigUint::ZERO { a.clone() } else { a % b }),
        NatGcd => n(a.gcd(b)),
        NatPow => {
            let e = small(b).filter(|e| *e <= MAX_POW_EXP as u64)?;
            n(a.pow(e as u32))
        }
        NatBeq => Some(Value::Bool(a == b)),
        NatBle => Some(Value::Bool(a <= b)),
        NatLand => n(a & b),
        NatLor => n(a | b),
        NatXor => n(a ^ b),
        NatShiftLeft => n(a << small(b)?),
        NatShiftRight => n(match small(b) {
            Some(s) => a >> s,
            None => BigUint::ZERO,
        }),
        _ => None,
    }
}
