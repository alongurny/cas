//! Checks for the outer protocol (spec/access.md).

use cas_core::algo::{self, Algorithm};
use cas_core::client::HttpStore;
use cas_core::mem::MemBackend;
use cas_core::server::Server;
use cas_core::{AllowAll, CoreStore, Error as CoreError, Expect, Op, Policy, Span, Store, Who};
use cas_version::access::{self, Access, Grant, Rules};
use cas_version::{Error, Node, Path, build, formats, history, put, sync};
use std::collections::BTreeMap;
use std::sync::Arc;

fn b3() -> &'static Algorithm {
    algo::lookup("blake3").unwrap()
}

fn store() -> Arc<dyn Store> {
    Arc::new(CoreStore::new(MemBackend::new(), AllowAll, Policy::permissive()).session(Who::anonymous()))
}

fn grant(rules: &mut Rules, who: &str, g: &str) {
    rules.grants.entry(who.to_string()).or_default().insert(Grant::parse(g).unwrap());
}

fn local_branch() -> Arc<dyn Store> {
    let s = store();
    let f = put(&*s, b3(), "raw", b"hello\n").unwrap();
    let m = BTreeMap::from([(Path::new("a").unwrap(), Node::File(f))]);
    let v = build(&*s, b3(), formats::by_alias("dir").unwrap(), &m).unwrap();
    history::commit(&*s, b3(), "main", &v, history::Step::default(), None).unwrap();
    s
}

#[test]
fn grants_and_rules_round_trip() {
    for g in ["read *", "write blobs", "list var:", "remove var:gc/"] {
        assert_eq!(Grant::parse(g).unwrap().to_string(), g);
    }
    assert!(Grant::parse("read").is_err() && Grant::parse("fly blobs").is_err() && Grant::parse("read main").is_err());
    let mut r = Rules::default();
    let token = r.add_user("alice").unwrap();
    grant(&mut r, "alice", "write var:main");
    grant(&mut r, access::ANYONE, "read blobs");
    assert_eq!(Rules::from_value(&r.to_value()).unwrap(), r);
    assert_eq!(r.user_for_token(&token), Some("alice"));
    assert_eq!(r.user_for_token("guess"), None);
    // Only the hash is kept.
    assert!(!r.to_value().to_text().contains(&token));
    assert!(r.add_user("Alice!").is_err());
}

#[test]
fn list_grants_cover_only_spans_inside_their_scope() {
    let mut r = Rules::default();
    grant(&mut r, access::ANYONE, "list var:main");
    grant(&mut r, "bob", "list blobs");
    let span =
        |f: &str, t: Option<&str>| Span { from: Some(f.as_bytes().to_vec()), to: t.map(|t| t.as_bytes().to_vec()) };
    let anon = Who::anonymous();
    assert!(r.allows(&anon, &Op::List { span: &span("main", Some("maio")) }));
    assert!(r.allows(&anon, &Op::List { span: &span("main/x", Some("main/y")) }));
    assert!(!r.allows(&anon, &Op::List { span: &span("main", None) }));
    assert!(!r.allows(&anon, &Op::List { span: &Span::all() }));
    let key = b3().key(b"x");
    let mut end = key.clone().into_bytes();
    end.push(0);
    let has = Span { from: Some(key.into_bytes()), to: Some(end) };
    assert!(!r.allows(&anon, &Op::List { span: &has }));
    assert!(r.allows(&Who::named("bob"), &Op::List { span: &has }));
    // A variable prefix that blob keys could share never authorizes a list.
    grant(&mut r, "carol", "list var:bla");
    assert!(!r.allows(&Who::named("carol"), &Op::List { span: &span("bla", Some("blb")) }));
}

#[test]
fn served_store_enforces_the_policy_from_its_meta_store() {
    let meta = store();
    let mut rules = Rules::default();
    let alice = rules.add_user("alice").unwrap();
    let bob = rules.add_user("bob").unwrap();
    for g in ["read blobs", "list blobs", "read var:main"] {
        grant(&mut rules, access::ANYONE, g);
    }
    for g in ["write blobs", "write var:main"] {
        grant(&mut rules, "alice", g);
    }
    access::save(&*meta, b3(), None, &rules).unwrap();

    let gate = Access::new(Arc::clone(&meta));
    let core = CoreStore::new(MemBackend::new(), gate.clone(), Policy::permissive());
    let server = Server::start(core, "127.0.0.1:0", 4, gate.authenticate()).unwrap();
    let url = server.url();
    let local = local_branch();

    let anon = HttpStore::new(&url);
    assert!(matches!(sync::push(&*local, &anon, "main", "main"), Err(Error::Store(CoreError::Denied))));
    let as_bob = HttpStore::new(&url).with_token(&bob);
    assert!(matches!(sync::push(&*local, &as_bob, "main", "main"), Err(Error::Store(CoreError::Denied))));
    let as_alice = HttpStore::new(&url).with_token(&alice);
    assert_eq!(sync::push(&*local, &as_alice, "main", "main").unwrap().0, sync::Outcome::Created);
    // Anyone may read what alice pushed.
    assert_eq!(history::load(&anon, "main").unwrap().1.log().len(), 1);
    // A wrong token is just anonymous.
    let guess = HttpStore::new(&url).with_token("0123");
    assert_eq!(guess.write("main", b"x", &Expect::Absent), Err(CoreError::Denied));
    // Alice may not write other variables.
    assert_eq!(as_alice.write("other", b"x", &Expect::Absent), Err(CoreError::Denied));

    // Changing the policy takes effect on the next request.
    let (old, mut rules) = access::load(&*meta).unwrap();
    rules.grants.remove("alice");
    access::save(&*meta, b3(), old, &rules).unwrap();
    let k = b3().key(b"new");
    assert_eq!(as_alice.write(&k, b"new", &Expect::Absent), Err(CoreError::Denied));
}

#[test]
fn an_unreadable_policy_denies_everything() {
    let meta = store();
    meta.write(access::VAR, b"not a view", &Expect::Absent).unwrap();
    let gate = Access::new(meta);
    let s = CoreStore::new(MemBackend::new(), gate, Policy::permissive()).session(Who::anonymous());
    assert_eq!(s.read("main"), Err(CoreError::Denied));
}
