//! Citing constants on demand: a builder with a `Source` cites a constant
//! the first time a term names it (its type, information and, for a
//! recursor, its rules), and a definition's value only when it is
//! unfolded.

use super::{Builder, Res};
use crate::expr::X;
use crate::tables::*;

/// Lean's reducibility hint of a definition; anything else is opaque.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Hint {
    #[default]
    Opaque,
    Abbrev,
    Regular(u32),
}

/// Lean's `compare` (kernel/declaration.cpp): below zero unfold the first,
/// above zero the second, zero both.
pub fn compare(a: Hint, b: Hint) -> i32 {
    use Hint::*;
    match (a, b) {
        (Regular(x), Regular(y)) => {
            if x == y {
                0
            } else if x > y {
                -1
            } else {
                1
            }
        }
        (Opaque, Opaque) | (Abbrev, Abbrev) => 0,
        (Opaque, _) => 1,
        (_, Opaque) => -1,
        (Abbrev, _) => -1,
        (_, Abbrev) => 1,
    }
}

/// What a constant's row is made of.
pub struct Cited {
    pub kind: DeclKind,
    pub levels: u32,
    pub ty: X,
    pub value: Option<X>,
    pub hint: Hint,
    /// The information, with a recursor's rules left empty: they are
    /// `rules`.
    pub info: Info,
    pub rules: Vec<(Key, u32, X)>,
    pub prim: Option<Prim>,
}

pub trait Source {
    fn cite(&self, key: &Key) -> Option<Cited>;
    /// The constant's name, for messages.
    fn name(&self, _key: &Key) -> Option<String> {
        None
    }
}

impl Builder {
    /// The row of the constant with this key, cited from the source if it
    /// is not yet.
    pub fn cite(&mut self, key: &Key) -> Res<Ix> {
        if let Some(d) = self.decls.get(key) {
            return Ok(*d);
        }
        let src = self.source.clone().ok_or("a constant is not cited")?;
        let c = src.cite(key).ok_or("a constant the source does not have")?;
        let ty = self.row_of(&c.ty)?;
        if let Some(d) = self.decls.get(key) {
            return Ok(*d);
        }
        let d = self.decl(DeclRow { key: *key, kind: c.kind, levels: c.levels, ty, value: None, info: c.info });
        self.hints.insert(d, c.hint);
        if let Some(v) = c.value {
            self.pending.insert(d, v);
        }
        if let Some(p) = c.prim {
            self.prim(p, d);
        }
        if !c.rules.is_empty() {
            let mut rules = vec![];
            for (ctor, fields, rhs) in &c.rules {
                rules.push(RecRule { ctor: *ctor, fields: *fields, rhs: self.row_of(rhs)? });
            }
            if let Info::Rec { rules: r, .. } = &mut self.t.decls[d as usize].info {
                *r = rules;
            }
        }
        Ok(d)
    }

    /// The value of the definition d, converted to rows when first needed.
    pub fn value(&mut self, d: Ix) -> Option<Ix> {
        if let Some(v) = self.t.decls[d as usize].value {
            return Some(v);
        }
        let x = self.pending.remove(&d)?;
        let v = self.row_of(&x).ok()?;
        self.t.decls[d as usize].value = Some(v);
        Some(v)
    }

    /// The hint of the constant row d.
    pub fn hint(&self, d: Ix) -> Hint {
        match self.t.decls[d as usize].kind {
            DeclKind::Definition => self.hints.get(&d).copied().unwrap_or(Hint::Regular(0)),
            _ => Hint::Opaque,
        }
    }
}

impl Builder {
    /// Term e, printed for messages (depth-limited).
    pub fn show(&self, e: Ix) -> String {
        fn go(b: &Builder, e: Ix, d: u32, out: &mut String) {
            if out.len() > 600 {
                return;
            }
            if d > 12 {
                out.push('…');
                return;
            }
            match &b.t.terms[e as usize] {
                TermRow::BVar(n) => out.push_str(&format!("#{n}")),
                TermRow::FVar(n) => out.push_str(&format!("x{n}")),
                TermRow::Sort(l) => out.push_str(&format!("Sort {:?}", b.to_level(*l))),
                TermRow::Const(k, _) => {
                    let key = b.t.decls[*k as usize].key;
                    let n = b
                        .source
                        .as_ref()
                        .and_then(|s| s.name(&key))
                        .unwrap_or_else(|| format!("c{:02x}{:02x}", key[0], key[1]));
                    out.push_str(&n)
                }
                TermRow::App(f, x) => {
                    out.push('(');
                    go(b, *f, d + 1, out);
                    out.push(' ');
                    go(b, *x, d + 1, out);
                    out.push(')');
                }
                TermRow::Lam(a, x) | TermRow::Pi(a, x) => {
                    out.push_str(if matches!(b.t.terms[e as usize], TermRow::Lam(..)) { "(fun " } else { "(Pi " });
                    go(b, *a, d + 1, out);
                    out.push_str(", ");
                    go(b, *x, d + 1, out);
                    out.push(')');
                }
                TermRow::Let(_, v, x) => {
                    out.push_str("(let ");
                    go(b, *v, d + 1, out);
                    out.push_str("; ");
                    go(b, *x, d + 1, out);
                    out.push(')');
                }
                TermRow::Lit(n) => out.push_str(&format!("{}", crate::nat::value(n))),
                TermRow::Str(s) => out.push_str(&format!("{:?}", String::from_utf8_lossy(s))),
                TermRow::Proj(_, i, x) => {
                    go(b, *x, d + 1, out);
                    out.push_str(&format!(".{i}"));
                }
            }
        }
        let mut out = String::new();
        go(self, e, 0, &mut out);
        out
    }
}
