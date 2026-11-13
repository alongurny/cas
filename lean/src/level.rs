//! Universe levels and their equivalence, as Lean's kernel decides it
//! (kernel/level.cpp: `normalize` and `is_equivalent`). Two levels are
//! equivalent when they are equal or have equal normal forms.

use std::cmp::Ordering;
use std::rc::Rc;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Level {
    Zero,
    Succ(Rc<Level>),
    Max(Rc<Level>, Rc<Level>),
    IMax(Rc<Level>, Rc<Level>),
    /// A level parameter, by its position in the declaration's list.
    Param(u32),
}

use Level::*;

impl Level {
    pub fn succ(self) -> Level {
        Succ(Rc::new(self))
    }
    pub fn max(a: Level, b: Level) -> Level {
        Max(Rc::new(a), Rc::new(b))
    }
    pub fn imax(a: Level, b: Level) -> Level {
        IMax(Rc::new(a), Rc::new(b))
    }

    /// Lean's order of kinds: zero, succ, max, imax, param.
    fn kind(&self) -> u8 {
        match self {
            Zero => 0,
            Succ(_) => 1,
            Max(..) => 2,
            IMax(..) => 3,
            Param(_) => 4,
        }
    }

    /// The level under its successors, and how many there are.
    pub fn to_offset(&self) -> (&Level, u32) {
        let mut l = self;
        let mut k = 0;
        while let Succ(x) = l {
            l = x;
            k += 1;
        }
        (l, k)
    }

    fn is_explicit(&self) -> bool {
        matches!(self.to_offset().0, Zero)
    }

    pub fn is_not_zero(&self) -> bool {
        match self {
            Zero | Param(_) => false,
            Succ(_) => true,
            Max(a, b) => a.is_not_zero() || b.is_not_zero(),
            IMax(_, b) => b.is_not_zero(),
        }
    }

    fn with_succ(mut self, k: u32) -> Level {
        for _ in 0..k {
            self = self.succ();
        }
        self
    }
}

/// `mk_max` of level.cpp: max with the obvious simplifications.
fn mk_max(a: Level, b: Level) -> Level {
    if a.is_explicit() && b.is_explicit() {
        return if a.to_offset().1 >= b.to_offset().1 { a } else { b };
    }
    if a == b {
        return a;
    }
    if a == Zero {
        return b;
    }
    if b == Zero {
        return a;
    }
    if let Max(x, y) = &b
        && (**x == a || **y == a)
    {
        return b;
    }
    if let Max(x, y) = &a
        && (**x == b || **y == b)
    {
        return a;
    }
    let ((pa, ka), (pb, kb)) = (a.to_offset(), b.to_offset());
    if pa == pb {
        return if ka > kb { a } else { b };
    }
    Level::max(a, b)
}

/// `mk_imax` of level.cpp.
fn mk_imax(a: Level, b: Level) -> Level {
    if b.is_not_zero() {
        mk_max(a, b)
    } else if b == Zero || a == Zero || a == Zero.succ() {
        // imax u 0 = 0; imax 0 u = imax 1 u = u.
        b
    } else if a == b {
        a
    } else {
        Level::imax(a, b)
    }
}

/// `is_norm_lt` of level.cpp.
fn norm_cmp(a: &Level, b: &Level) -> Ordering {
    let ((l1, k1), (l2, k2)) = (a.to_offset(), b.to_offset());
    if l1 != l2 {
        if l1.kind() != l2.kind() {
            return l1.kind().cmp(&l2.kind());
        }
        match (l1, l2) {
            (Param(x), Param(y)) => x.cmp(y),
            (Max(a1, b1), Max(a2, b2)) | (IMax(a1, b1), IMax(a2, b2)) => {
                if a1 != a2 {
                    norm_cmp(a1, a2)
                } else {
                    norm_cmp(b1, b2)
                }
            }
            _ => unreachable!("zero and succ are offsets"),
        }
    } else {
        k1.cmp(&k2)
    }
}

fn push_max_args(l: &Level, out: &mut Vec<Level>) {
    if let Max(a, b) = l {
        push_max_args(a, out);
        push_max_args(b, out);
    } else {
        out.push(l.clone());
    }
}

/// `normalize` of level.cpp.
pub fn normalize(l: &Level) -> Level {
    let (r, k) = l.to_offset();
    match r {
        Zero | Param(_) => l.clone(),
        Succ(_) => unreachable!(),
        IMax(a, b) => mk_imax(normalize(a), normalize(b)).with_succ(k),
        Max(..) => {
            let mut todo = Vec::new();
            push_max_args(r, &mut todo);
            let mut args = Vec::new();
            for a in &todo {
                push_max_args(&normalize(a), &mut args);
            }
            args.sort_by(norm_cmp);
            let mut i = 0;
            if args[i].is_explicit() {
                while i + 1 < args.len() && args[i + 1].is_explicit() {
                    i += 1;
                }
                let kk = args[i].to_offset().1;
                // An explicit level is subsumed by a succ^k'(l) with k' >= k.
                if args[i + 1..].iter().any(|a| a.to_offset().1 >= kk) {
                    i += 1;
                }
            }
            let mut rargs: Vec<Level> = vec![args[i].clone()];
            let mut prev = args[i].clone();
            for a in &args[i + 1..] {
                let (pp, pk) = prev.to_offset();
                let (cp, ck) = a.to_offset();
                if pp == cp {
                    if pk < ck {
                        prev = a.clone();
                        rargs.pop();
                        rargs.push(a.clone());
                    }
                } else {
                    prev = a.clone();
                    rargs.push(a.clone());
                }
            }
            let rargs: Vec<Level> = rargs.into_iter().map(|a| a.with_succ(k)).collect();
            // mk_max of a list: right-nested, built from the end.
            let mut it = rargs.into_iter().rev();
            let mut acc = it.next().expect("at least one");
            for a in it {
                acc = mk_max(a, acc);
            }
            acc
        }
    }
}

/// `is_equivalent` of level.cpp.
pub fn equivalent(a: &Level, b: &Level) -> bool {
    a == b || normalize(a) == normalize(b)
}

fn is_zero(l: &Level) -> bool {
    *l == Zero
}

fn geq_core(l1: &Level, l2: &Level) -> bool {
    if l1 == l2 || is_zero(l2) {
        return true;
    }
    if let Max(a, b) = l2 {
        return geq(l1, a) && geq(l1, b);
    }
    if let Max(a, b) = l1
        && (geq(a, l2) || geq(b, l2))
    {
        return true;
    }
    if let IMax(a, b) = l2 {
        return geq(l1, a) && geq(l1, b);
    }
    if let IMax(_, b) = l1 {
        return geq(b, l2);
    }
    let ((p1, k1), (p2, k2)) = (l1.to_offset(), l2.to_offset());
    if p1 == p2 || is_zero(p2) {
        return k1 >= k2;
    }
    if k1 == k2 && k1 > 0 {
        return geq(p1, p2);
    }
    false
}

/// `is_geq` of level.cpp: `l1` is at least `l2` for every assignment, as
/// far as Lean's kernel can tell (it checks that constructor fields fit
/// their inductive type's universe).
pub fn geq(l1: &Level, l2: &Level) -> bool {
    geq_core(&normalize(l1), &normalize(l2))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_commutes_and_absorbs() {
        let (u, v) = (Param(0), Param(1));
        assert!(equivalent(&Level::max(u.clone(), v.clone()), &Level::max(v.clone(), u.clone())));
        assert!(equivalent(&Level::max(u.clone(), Zero), &u));
        assert!(equivalent(&Level::max(u.clone().succ(), u.clone()), &u.clone().succ()));
        assert!(equivalent(&Level::imax(u.clone(), Zero), &Zero));
        assert!(equivalent(&Level::imax(u.clone(), v.clone().succ()), &Level::max(u.clone(), v.clone().succ())));
        assert!(!equivalent(&u, &v));
        assert!(!equivalent(&u.clone().succ(), &u));
        assert!(geq(&Level::max(u.clone(), v.clone()), &u));
        assert!(geq(&u.clone().succ(), &u));
        assert!(!geq(&u, &v));
        assert!(geq(&Level::imax(u.clone(), v.clone()), &v));
    }
}
