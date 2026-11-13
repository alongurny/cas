//! The prover's side of a nested inductive block: Lean's `elim` into a
//! mutual block, that block's check, and its recursors restored
//! (crate::nested).

use super::{Builder, Res};
use crate::expr::{E, X, from_row};
use crate::level::Level;
use crate::nested::*;
use crate::tables::*;
use std::collections::HashMap;

impl Builder {
    fn level_row(&mut self, l: &Level) -> Ix {
        let r = match l {
            Level::Zero => LevelRow::Zero,
            Level::Succ(a) => LevelRow::Succ(self.level_row(a)),
            Level::Max(a, b) => LevelRow::Max(self.level_row(a), self.level_row(b)),
            Level::IMax(a, b) => LevelRow::IMax(self.level_row(a), self.level_row(b)),
            Level::Param(n) => LevelRow::Param(*n),
        };
        self.level(r)
    }

    /// The row of e; the constants it names are cited.
    pub(crate) fn row_of(&mut self, e: &X) -> Res<Ix> {
        let d = |b: &Builder, k: &Key| b.decls.get(k).copied().ok_or_else(|| "a constant is not cited".to_string());
        let r = match &**e {
            E::BVar(n) => TermRow::BVar(*n),
            E::FVar(n) => TermRow::FVar(*n),
            E::Sort(l) => TermRow::Sort(self.level_row(l)),
            E::Const(k, ls) => {
                let ls: Vec<Ix> = ls.iter().map(|l| self.level_row(l)).collect();
                TermRow::Const(d(self, k)?, self.list(&ls))
            }
            E::App(a, b) => TermRow::App(self.row_of(a)?, self.row_of(b)?),
            E::Lam(a, b) => TermRow::Lam(self.row_of(a)?, self.row_of(b)?),
            E::Pi(a, b) => TermRow::Pi(self.row_of(a)?, self.row_of(b)?),
            E::Let(a, v, b) => TermRow::Let(self.row_of(a)?, self.row_of(v)?, self.row_of(b)?),
            E::Lit(i) => return Ok(*i),
            E::Proj(k, n, x) => TermRow::Proj(d(self, k)?, *n, self.row_of(x)?),
        };
        Ok(self.term(r))
    }

    /// Checks the nested inductive block whose types are the cited rows
    /// `types`, with constructors `ctors`, as Lean's kernel does: the
    /// mutual block `elim` makes of it is checked, then its types'
    /// information is set and its recursors are cited under `rec_keys`
    /// (one per type of the mutual block) restored.
    pub fn nested_block(
        &mut self,
        levels: u32,
        params: u32,
        types: &[Ix],
        ctors: &[Vec<Ix>],
        rec_keys: &[Key],
    ) -> Res<NestedGoal> {
        let mut memo = HashMap::new();
        let mut own = vec![];
        for (d, cs) in types.iter().zip(ctors) {
            let row = self.t.decls[*d as usize].clone();
            let cs = cs
                .iter()
                .map(|c| {
                    let c = &self.t.decls[*c as usize];
                    (c.key, from_row(&self.t, c.ty, &mut memo))
                })
                .collect();
            own.push(Ty { key: row.key, ty: from_row(&self.t, row.ty, &mut memo), ctors: cs });
        }
        let el = elim(&self.t, levels, params as usize, &own)?;
        if el.types.len() == own.len() {
            return Err("nothing is nested".into());
        }
        if rec_keys.len() != el.types.len() {
            return Err(format!("the mutual block has {} types", el.types.len()));
        }
        // Cite the mutual block's types, then its constructors (which
        // mention the types).
        let mut aux_types = vec![];
        for (i, ty) in el.types.iter().enumerate() {
            let t = if i < types.len() { self.t.decls[types[i] as usize].ty } else { self.row_of(&ty.ty)? };
            aux_types.push(self.decl(DeclRow {
                key: ty.key,
                kind: DeclKind::Inductive,
                levels,
                ty: t,
                value: None,
                info: Info::None,
            }));
        }
        let mut aux_ctors = vec![];
        for ty in &el.types {
            let mut cs = vec![];
            for (k, cty) in &ty.ctors {
                let t = self.row_of(cty)?;
                cs.push(self.decl(DeclRow {
                    key: *k,
                    kind: DeclKind::Constructor,
                    levels,
                    ty: t,
                    value: None,
                    info: Info::None,
                }));
            }
            aux_ctors.push(cs);
        }
        let aux_rec_keys: Vec<Key> = rec_keys.iter().map(copy_key).collect();
        let aux = self.inductive_block(levels, params, &aux_types, &aux_ctors, &aux_rec_keys)?;
        // The block's own types and constructors take the copies'
        // information.
        let keys: Vec<Key> = types.iter().map(|d| self.t.decls[*d as usize].key).collect();
        for (i, d) in types.iter().enumerate() {
            let Info::Induct { params, indices, recursive, .. } = self.t.decls[aux_types[i] as usize].info.clone()
            else {
                unreachable!()
            };
            let cks: Vec<Key> = ctors[i].iter().map(|c| self.t.decls[*c as usize].key).collect();
            self.t.decls[*d as usize].info = Info::Induct { params, indices, ctors: cks, recursive, all: keys.clone() };
            for (c, cc) in ctors[i].iter().zip(&aux_ctors[i]) {
                let Info::Ctor { params, fields, .. } = self.t.decls[*cc as usize].info else { unreachable!() };
                self.t.decls[*c as usize].info = Info::Ctor { induct: keys[i], params, fields };
            }
        }
        // The recursors, restored: cited first, since the rules name them.
        let recs: HashMap<Key, Key> = aux_rec_keys.iter().copied().zip(rec_keys.iter().copied()).collect();
        let mut out = vec![];
        for (r2, k) in aux.recursors.iter().zip(rec_keys) {
            let src = self.t.decls[*r2 as usize].clone();
            let shift = src.levels - levels;
            let ty = el.restore(&from_row(&self.t, src.ty, &mut memo), shift, &recs)?;
            let ty = self.row_of(&ty)?;
            out.push(self.decl(DeclRow {
                key: *k,
                kind: DeclKind::Recursor,
                levels: src.levels,
                ty,
                value: None,
                info: Info::None,
            }));
        }
        for (r2, r) in aux.recursors.iter().zip(&out) {
            let src = self.t.decls[*r2 as usize].clone();
            let shift = src.levels - levels;
            let Info::Rec { params, motives, minors, indices, rules } = src.info else { unreachable!() };
            let mut new_rules = vec![];
            for rule in rules {
                let rhs = el.restore(&from_row(&self.t, rule.rhs, &mut memo), shift, &recs)?;
                let ctor = el.original(&rule.ctor).ok_or("a rule of no constructor")?;
                new_rules.push(RecRule { ctor, fields: rule.fields, rhs: self.row_of(&rhs)? });
            }
            self.t.decls[*r as usize].info = Info::Rec { params, motives, minors, indices, rules: new_rules };
        }
        Ok(NestedGoal { levels, params, types: types.to_vec(), recursors: out, aux })
    }
}
