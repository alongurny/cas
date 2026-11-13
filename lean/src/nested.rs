//! Nested inductive types (spec/lean/tables.md, "nested inductive
//! blocks"), as Lean's kernel handles them (kernel/inductive.cpp,
//! `add_inductive`): the block is rewritten into a mutual block with an
//! auxiliary type for each nested occurrence (`elim`), that block is
//! checked as any other, and its recursors are rewritten back (`restore`).
//! Both rewritings are syntactic; this module is shared by the prover and
//! the checker, which compares their results by id.
//!
//! Lean names the auxiliary types; here they are keys derived from the
//! block's first type. The mutual block also holds copies of the block's
//! own types and constructors under derived keys, since their constructors'
//! types differ there (they mention the auxiliary types).

use crate::expr::*;
use crate::level::Level;
use crate::tables::*;
use std::collections::HashMap;
use std::rc::Rc;

fn derive(ctx: &str, parts: &[&[u8]]) -> Key {
    let mut h = blake3::Hasher::new_derive_key(ctx);
    for p in parts {
        h.update(p);
    }
    *h.finalize().as_bytes()
}

/// The key of a type, constructor or recursor of the block in the mutual
/// block.
pub fn copy_key(k: &Key) -> Key {
    derive("cas-lean nested copy", &[k])
}

/// The key of the n-th auxiliary type of the block whose first type is
/// `first`, and of its j-th constructor.
pub fn aux_key(first: &Key, n: u32) -> Key {
    derive("cas-lean nested aux type", &[first, &n.to_le_bytes()])
}
pub fn aux_ctor_key(aux: &Key, j: u32) -> Key {
    derive("cas-lean nested aux constructor", &[aux, &j.to_le_bytes()])
}

/// A type of a block: its key, its type and its constructors.
#[derive(Clone, Debug)]
pub struct Ty {
    pub key: Key,
    pub ty: X,
    pub ctors: Vec<(Key, X)>,
}

/// The mutual block and what `restore` needs to undo it.
#[derive(Clone, Debug)]
pub struct Elim {
    /// The block's own types (under their copy keys), then the auxiliary
    /// ones, in Lean's order.
    pub types: Vec<Ty>,
    pub ntypes: usize,
    /// The block's parameters as the local constants `nested` mentions.
    params: Vec<u32>,
    /// Each auxiliary type: the nested occurrence `I Ds` it stands for.
    aux2nested: HashMap<Key, X>,
    /// Each auxiliary constructor: its type's nested occurrence and the
    /// constructor of `I` it copies.
    ctor2nested: HashMap<Key, (X, Key)>,
    /// Copy key to the block's own key.
    uncopy: HashMap<Key, Key>,
}

struct St<'a> {
    t: &'a Tables,
    np: usize,
    lvls: Vec<Level>,
    first: Key,
    next_aux: u32,
    next_fvar: u32,
    types: Vec<Ty>,
    /// (I Ds over the block's parameters, auxiliary type)
    nested_aux: Vec<(X, Key)>,
    params: Vec<u32>,
    aux2nested: HashMap<Key, X>,
    ctor2nested: HashMap<Key, (X, Key)>,
}

fn decl_by_key<'a>(t: &'a Tables, k: &Key) -> Option<&'a DeclRow> {
    t.decls.iter().find(|d| &d.key == k)
}

/// Peels n binders off e, as fresh local constants with their types.
fn peel(e: &X, n: usize, next: &mut u32) -> Result<(Vec<(u32, X)>, X), String> {
    let mut xs: Vec<(u32, X)> = vec![];
    let mut e = e.clone();
    for _ in 0..n {
        let (a, b) = match &*e {
            E::Pi(a, b) | E::Lam(a, b) => (a.clone(), b.clone()),
            _ => return Err("too few parameters".into()),
        };
        let x = *next;
        *next += 1;
        xs.push((x, a));
        e = instantiate_rev(&b, &[Rc::new(E::FVar(x))]);
    }
    Ok((xs, e))
}

fn fvars(xs: &[(u32, X)]) -> Vec<X> {
    xs.iter().map(|(x, _)| Rc::new(E::FVar(*x))).collect()
}

/// `instantiate_pi_params`: the first n binders of e (pis) taken by `vals`.
fn instantiate_pi_params(e: &X, n: usize, vals: &[X]) -> Result<X, String> {
    let mut e = e.clone();
    for _ in 0..n {
        let E::Pi(_, b) = &*e else { return Err("invalid nested inductive datatype, ill-formed declaration".into()) };
        e = b.clone();
    }
    Ok(instantiate_rev(&e, &vals[..n]))
}

impl St<'_> {
    fn replace_params(&self, e: &X, xs: &[(u32, X)]) -> X {
        let ids: Vec<u32> = xs.iter().map(|x| x.0).collect();
        let ps: Vec<X> = self.params.iter().map(|p| Rc::new(E::FVar(*p))).collect();
        instantiate_rev(&abstract_(e, &ids), &ps)
    }

    /// `is_nested_inductive_app`: e is `I Ds is` with I an inductive type
    /// declared before, whose parameters Ds mention the block. Returns I's
    /// row.
    fn is_nested_app(&self, e: &X) -> Result<Option<&DeclRow>, String> {
        if !matches!(**e, E::App(..)) {
            return Ok(None);
        }
        let (f, args) = spine(e);
        let E::Const(k, _) = &*f else { return Ok(None) };
        if self.types.iter().any(|t| &t.key == k) {
            return Ok(None);
        }
        let Some(row) = decl_by_key(self.t, k) else { return Ok(None) };
        let Info::Induct { params, .. } = &row.info else { return Ok(None) };
        if row.kind != DeclKind::Inductive || *params as usize > args.len() {
            return Ok(None);
        }
        let ds = &args[..*params as usize];
        let keys: Vec<Key> = self.types.iter().map(|t| t.key).collect();
        let nested = ds.iter().any(|a| find(a, &|x| matches!(x, E::Const(c, _) if keys.contains(c))));
        if !nested {
            return Ok(None);
        }
        if ds.iter().any(has_loose_bvars) {
            return Err("invalid nested inductive datatype, nested inductive datatypes parameters cannot contain local variables".into());
        }
        Ok(Some(row))
    }

    /// `replace_if_nested`: a nested occurrence `I Ds is` becomes
    /// `Iaux As is`, creating the auxiliary types on first sight.
    fn replace_if_nested(&mut self, xs: &[(u32, X)], e: &X) -> Result<Option<X>, String> {
        let Some(row) = self.is_nested_app(e)? else { return Ok(None) };
        let row = row.clone();
        let Info::Induct { params: inp, all, .. } = &row.info else { unreachable!() };
        let inp = *inp as usize;
        let (f, args) = spine(e);
        let E::Const(i_key, i_lvls) = &*f else { unreachable!() };
        let as_: Vec<X> = fvars(xs);
        let aux_app = |k: Key, lvls: &[Level]| {
            let c = mk_app(Rc::new(E::Const(k, lvls.to_vec())), &as_);
            mk_app(c, &args[inp..])
        };
        let iparams = self.replace_params(&mk_app(f.clone(), &args[..inp]), xs);
        if let Some((_, k)) = self.nested_aux.iter().find(|(n, _)| *n == iparams) {
            return Ok(Some(aux_app(*k, &self.lvls)));
        }
        let mut result = None;
        for j_key in all {
            let j = decl_by_key(self.t, j_key).ok_or("a type of the nested type's block is not cited")?;
            let Info::Induct { params: jp, ctors, .. } = &j.info else { return Err("not an inductive type".into()) };
            if *jp as usize != inp || j.levels as usize != i_lvls.len() {
                return Err("the nested type's block disagrees on its parameters".into());
            }
            let j_const = Rc::new(E::Const(*j_key, i_lvls.clone()));
            self.next_aux += 1;
            let aux = aux_key(&self.first, self.next_aux);
            let mut memo = HashMap::new();
            let jty = inst_levels(&from_row(self.t, j.ty, &mut memo), i_lvls);
            let aux_ty = bind(true, xs, &instantiate_pi_params(&jty, inp, &args)?);
            let nested = self.replace_params(&mk_app(j_const, &args[..inp]), xs);
            self.nested_aux.push((nested.clone(), aux));
            self.aux2nested.insert(aux, nested.clone());
            if j_key == i_key {
                result = Some(aux_app(aux, &self.lvls));
            }
            let mut cs = vec![];
            for (n, c_key) in ctors.iter().enumerate() {
                let c = decl_by_key(self.t, c_key).ok_or("a constructor of the nested type is not cited")?;
                let cty = inst_levels(&from_row(self.t, c.ty, &mut memo), i_lvls);
                let k = aux_ctor_key(&aux, n as u32);
                cs.push((k, bind(true, xs, &instantiate_pi_params(&cty, inp, &args)?)));
                self.ctor2nested.insert(k, (nested.clone(), *c_key));
            }
            self.types.push(Ty { key: aux, ty: aux_ty, ctors: cs });
        }
        Ok(result)
    }
}

/// `elim_nested_inductive`: the mutual block for the block `types`, with
/// `np` parameters and `levels` level parameters. `ntypes` of the result
/// equals the length of `types` when nothing is nested.
pub fn elim(t: &Tables, levels: u32, np: usize, types: &[Ty]) -> Result<Elim, String> {
    let first = types.first().ok_or("an empty block")?.key;
    let mut st = St {
        t,
        np,
        lvls: (0..levels).map(Level::Param).collect(),
        first,
        next_aux: 0,
        next_fvar: 0,
        types: types.to_vec(),
        nested_aux: vec![],
        params: vec![],
        aux2nested: HashMap::new(),
        ctor2nested: HashMap::new(),
    };
    let (ps, _) = peel(&types[0].ty, np, &mut st.next_fvar)?;
    st.params = ps.iter().map(|p| p.0).collect();
    let mut q = 0;
    while q < st.types.len() {
        let mut cs = vec![];
        for (k, cty) in st.types[q].ctors.clone() {
            let (xs, body) = peel(&cty, st.np, &mut st.next_fvar)?;
            let mut err = None;
            let body = replace(&body, 0, &mut |e, _| match st.replace_if_nested(&xs, e) {
                Ok(r) => r,
                Err(m) => {
                    err.get_or_insert(m);
                    None
                }
            });
            if let Some(m) = err {
                return Err(m);
            }
            cs.push((k, bind(true, &xs, &body)));
        }
        st.types[q].ctors = cs;
        q += 1;
    }
    // The block's own types and constructors under their copy keys.
    let own: HashMap<Key, Key> = types.iter().map(|t| (t.key, copy_key(&t.key))).collect();
    let rename = |e: &X| {
        replace(e, 0, &mut |x, _| match &**x {
            E::Const(k, ls) => own.get(k).map(|c| Rc::new(E::Const(*c, ls.clone()))),
            _ => None,
        })
    };
    let mut uncopy = HashMap::new();
    let mut out = vec![];
    for (i, ty) in st.types.iter().enumerate() {
        let own_type = i < types.len();
        let key = if own_type { copy_key(&ty.key) } else { ty.key };
        if own_type {
            uncopy.insert(key, ty.key);
        }
        let ctors = ty
            .ctors
            .iter()
            .map(|(k, c)| {
                let k2 = if own_type { copy_key(k) } else { *k };
                if own_type {
                    uncopy.insert(k2, *k);
                }
                (k2, rename(c))
            })
            .collect();
        out.push(Ty { key, ty: rename(&ty.ty), ctors });
    }
    Ok(Elim {
        types: out,
        ntypes: types.len(),
        params: st.params,
        aux2nested: st.aux2nested,
        ctor2nested: st.ctor2nested,
        uncopy,
    })
}

impl Elim {
    /// The block's own key of a key of the mutual block's own types and
    /// constructors, or the constructor of the nested type an auxiliary
    /// constructor copies.
    pub fn original(&self, k: &Key) -> Option<Key> {
        self.uncopy.get(k).copied().or_else(|| self.ctor2nested.get(k).map(|c| c.1))
    }

    /// `restore_nested`: e (closed, its first `np` binders the parameters)
    /// with the auxiliary types and constructors replaced by the nested
    /// occurrences, the copies by the block's own types, and the
    /// recursors renamed by `recs`. The level parameters of the nested
    /// occurrences move up by `shift`, as a recursor's do.
    pub fn restore(&self, e: &X, shift: u32, recs: &HashMap<Key, Key>) -> Result<X, String> {
        let np = self.params.len();
        let pi = matches!(**e, E::Pi(..));
        let mut next = self.params.iter().max().map_or(0, |m| m + 1) + 1_000_000;
        let (xs, body) = peel(e, np, &mut next)?;
        let as_ = fvars(&xs);
        let at = |nested: &X| instantiate_rev(&abstract_(&shift_levels(nested, shift), &self.params), &as_);
        let mut err = None;
        let body = replace(&body, 0, &mut |t, _| {
            if let E::Const(k, ls) = &**t
                && let Some(r) = recs.get(k).or_else(|| self.uncopy.get(k))
            {
                return Some(Rc::new(E::Const(*r, ls.clone())));
            }
            let (f, args) = spine(t);
            let E::Const(k, _) = &*f else { return None };
            if let Some(nested) = self.aux2nested.get(k) {
                if args.len() < np {
                    err.get_or_insert("an auxiliary type applied to too few parameters");
                    return None;
                }
                return Some(mk_app(at(nested), &args[np..]));
            }
            if let Some((nested, c)) = self.ctor2nested.get(k) {
                if args.len() < np {
                    err.get_or_insert("an auxiliary constructor applied to too few parameters");
                    return None;
                }
                let (i, i_args) = spine(&at(nested));
                let E::Const(_, ls) = &*i else { unreachable!() };
                let f = mk_app(Rc::new(E::Const(*c, ls.clone())), &i_args);
                return Some(mk_app(f, &args[np..]));
            }
            None
        });
        if let Some(m) = err {
            return Err(m.into());
        }
        Ok(bind(pi, &xs, &body))
    }
}
