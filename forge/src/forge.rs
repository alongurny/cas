//! The forge: one store, seen with full rights, and what is cached about it.

use crate::events::Hub;
use crate::names::ProjectId;
use crate::reach::Index;
use crate::record::Doc;
use cas_core::algo::{self, Algorithm};
use cas_core::{CoreStore, Store, Who};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

/// The caller name the forge uses for itself on its store. A `:` cannot be
/// in an account name, so no one else can be it.
pub const SELF: &str = ":forge";

#[derive(Default)]
pub struct Caches {
    pub accounts: Option<Arc<BTreeMap<String, Doc>>>,
    pub projects: Option<Arc<BTreeSet<ProjectId>>>,
    pub settings: BTreeMap<ProjectId, Arc<Doc>>,
    /// Histories lately read, newest last.
    pub histories: Vec<(cas_version::View, Arc<cas_version::history::History>)>,
}

pub struct Forge {
    /// The store, with the forge's own full rights.
    pub store: Arc<dyn Store>,
    pub core: Arc<CoreStore>,
    pub alg: &'static Algorithm,
    pub events: Hub,
    pub reach: Index,
    pub caches: Mutex<Caches>,
}

impl Forge {
    pub fn new(core: Arc<CoreStore>) -> Arc<Forge> {
        let store: Arc<dyn Store> = Arc::new(core.session(Who::named(SELF)));
        Arc::new(Forge {
            store,
            core,
            alg: algo::lookup("blake3").unwrap(),
            events: Hub::default(),
            reach: Index::default(),
            caches: Mutex::new(Caches::default()),
        })
    }

    /// Called after variable `var` moved from `old` to `new` (either may
    /// be absent), by `actor`, whoever moved it: on the wire or here.
    pub fn moved(&self, actor: Option<&str>, var: &str, old: Option<&[u8]>, new: Option<&[u8]>) {
        if var.starts_with("forge/a/") {
            self.caches.lock().unwrap().accounts = None;
            return;
        }
        let Some((project, rest)) = ProjectId::of_var(var) else { return };
        if rest == "meta" {
            let mut c = self.caches.lock().unwrap();
            c.settings.remove(&project);
            if old.is_none() || new.is_none() {
                c.projects = None;
            }
            return;
        }
        let view = new.and_then(|n| std::str::from_utf8(n).ok()).and_then(|s| cas_version::View::parse(s).ok());
        if let Some(v) = &view {
            self.reach.add(&*self.store, &project, v);
        }
        if rest.starts_with("u/") && new.is_some() {
            self.source_moved(&project, rest);
        }
        if rest.starts_with("h/") || rest.starts_with("u/") {
            let text = |b: Option<&[u8]>| b.map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default();
            let (o, n) = (text(old), text(new));
            let rest = rest.to_string();
            self.event("move", actor, |e| {
                e.set("project", project.to_string())
                    .set("var", rest.clone())
                    .set("old", o.clone())
                    .set("new", n.clone());
            });
        }
    }

    /// Every project, by its `meta` variable.
    pub fn projects(&self) -> crate::Result<Arc<BTreeSet<ProjectId>>> {
        if let Some(p) = self.caches.lock().unwrap().projects.clone() {
            return Ok(p);
        }
        let mut out = BTreeSet::new();
        for var in self.store.list_all(&cas_core::Span::prefix(b"p/"))? {
            if let Some((p, "meta")) = ProjectId::of_var(&var) {
                out.insert(p);
            }
        }
        let p = Arc::new(out);
        self.caches.lock().unwrap().projects = Some(p.clone());
        Ok(p)
    }
}
