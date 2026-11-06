use cas_core::cache::CachingStore;
use cas_core::client::HttpStore;
use cas_core::conformance::Harness;
use cas_core::disk::DiskBackend;
use cas_core::mem::MemBackend;
use cas_core::server::Server;
use cas_core::{AllowAll, CoreStore, Error, Expect, Gate, Op, Policy, Span, Store, Who};
use std::sync::Arc;

fn mem(policy: Policy, gate: Box<dyn Gate>) -> Arc<dyn Store> {
    Arc::new(CoreStore::with_gate(MemBackend::new(), gate.into(), policy).session(Who::anonymous()))
}

fn disk(policy: Policy, gate: Box<dyn Gate>) -> Arc<dyn Store> {
    // Leaked so the directory outlives the store for the test's duration.
    let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
    Arc::new(
        CoreStore::with_gate(DiskBackend::open(dir.path()).unwrap(), gate.into(), policy).session(Who::anonymous()),
    )
}

/// A server over a fresh in-memory store, and an HTTP client for it.
fn serve(policy: Policy, gate: Arc<dyn Gate>) -> Arc<dyn Store> {
    let core = CoreStore::with_gate(MemBackend::new(), gate, policy);
    let server = Server::start(core, "127.0.0.1:0", 32, Arc::new(|_: &tiny_http::Request| Who::anonymous())).unwrap();
    let url = server.url();
    Box::leak(Box::new(server));
    Arc::new(HttpStore::new(url))
}

fn wire(policy: Policy, gate: Box<dyn Gate>) -> Arc<dyn Store> {
    serve(policy, gate.into())
}

fn local() -> Arc<dyn Store> {
    Arc::new(CoreStore::new(MemBackend::new(), AllowAll, Policy::permissive()).session(Who::anonymous()))
}

fn cached(policy: Policy, gate: Box<dyn Gate>) -> Arc<dyn Store> {
    let gate: Arc<dyn Gate> = gate.into();
    let upstream =
        Arc::new(CoreStore::with_gate(MemBackend::new(), Arc::clone(&gate), policy).session(Who::anonymous()));
    Arc::new(CachingStore::new(local(), upstream, gate, Who::anonymous()))
}

/// Client -> cache -> HTTP -> server.
fn cached_wire(policy: Policy, gate: Box<dyn Gate>) -> Arc<dyn Store> {
    let gate: Arc<dyn Gate> = gate.into();
    let upstream = serve(policy, Arc::clone(&gate));
    Arc::new(CachingStore::new(local(), upstream, gate, Who::anonymous()))
}

mod mem_backend {
    cas_core::conformance_tests!(super::mem as fn(_, _) -> _);
}

mod disk_backend {
    cas_core::conformance_tests!(super::disk as fn(_, _) -> _);
}

mod over_wire {
    cas_core::conformance_tests!(super::wire as fn(_, _) -> _);
}

mod through_cache {
    cas_core::conformance_tests!(super::cached as fn(_, _) -> _);
}

mod through_cache_over_wire {
    cas_core::conformance_tests!(super::cached_wire as fn(_, _) -> _);
}

#[test]
fn disk_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let k = cas_core::algo::lookup("blake3").unwrap().key(b"durable");
    {
        let s = CoreStore::new(DiskBackend::open(dir.path()).unwrap(), AllowAll, Policy::permissive())
            .session(Who::anonymous());
        s.write(&k, b"durable", &Expect::Absent).unwrap();
        s.write("main", &cas_core::conformance::view(b"durable"), &Expect::Absent).unwrap();
    }
    let s = CoreStore::new(DiskBackend::open(dir.path()).unwrap(), AllowAll, Policy::permissive())
        .session(Who::anonymous());
    assert_eq!(s.read("main").unwrap(), Some(cas_core::conformance::view(b"durable")));
    assert_eq!(s.read(&k).unwrap(), Some(b"durable".to_vec()));
    let _: &dyn Harness = &(disk as fn(_, _) -> _);
}

/// An upstream that counts reads and can lie about blob bytes.
struct Upstream {
    inner: Arc<dyn Store>,
    reads: std::sync::atomic::AtomicUsize,
    corrupt: bool,
}

impl Store for Upstream {
    fn read(&self, key: &str) -> cas_core::Result<Option<Vec<u8>>> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let v = self.inner.read(key)?;
        Ok(if self.corrupt {
            v.map(|mut v| {
                v.push(b'!');
                v
            })
        } else {
            v
        })
    }
    fn write(&self, key: &str, value: &[u8], expect: &Expect) -> cas_core::Result<()> {
        self.inner.write(key, value, expect)
    }
    fn remove(&self, key: &str, expect: &Expect) -> cas_core::Result<()> {
        self.inner.remove(key, expect)
    }
    fn list(&self, span: &Span) -> cas_core::Result<cas_core::ListPage> {
        self.inner.list(span)
    }
}

fn counted(corrupt: bool) -> Arc<Upstream> {
    Arc::new(Upstream { inner: local(), reads: Default::default(), corrupt })
}

#[test]
fn cache_answers_blob_reads_and_forwards_var_reads() {
    let up = counted(false);
    let k = cas_core::algo::lookup("blake3").unwrap().key(b"v");
    up.inner.write(&k, b"v", &Expect::Absent).unwrap();
    let (v1, v2) = (cas_core::conformance::view(b"1"), cas_core::conformance::view(b"2"));
    up.inner.write("main", &v1, &Expect::Absent).unwrap();
    let cache = CachingStore::new(local(), up.clone(), Arc::new(AllowAll), Who::anonymous());
    let reads = || up.reads.load(std::sync::atomic::Ordering::SeqCst);

    assert_eq!(cache.read(&k).unwrap(), Some(b"v".to_vec()));
    assert_eq!(cache.read(&k).unwrap(), Some(b"v".to_vec()));
    assert_eq!(reads(), 1, "second blob read is served locally");

    assert_eq!(cache.read("main").unwrap(), Some(v1.clone()));
    up.inner.write("main", &v2, &Expect::Value(v1)).unwrap();
    assert_eq!(cache.read("main").unwrap(), Some(v2), "variable reads are never stale");
}

#[test]
fn cache_rejects_corrupt_upstream_bytes() {
    let up = counted(true);
    let k = cas_core::algo::lookup("blake3").unwrap().key(b"v");
    up.inner.write(&k, b"v", &Expect::Absent).unwrap();
    let local = local();
    let cache = CachingStore::new(Arc::clone(&local), up, Arc::new(AllowAll), Who::anonymous());
    assert!(matches!(cache.read(&k), Err(Error::Unavailable(_))));
    assert_eq!(local.read(&k).unwrap(), None, "nothing corrupt is kept");
}

#[test]
fn cache_never_serves_a_denied_read() {
    let k = cas_core::algo::lookup("blake3").unwrap().key(b"secret");
    let gate: Arc<dyn Gate> = Arc::new(|who: &Who, op: &Op<'_>| !matches!(op, Op::Read { .. }) || who.0.is_some());
    let upstream = local();
    upstream.write(&k, b"secret", &Expect::Absent).unwrap();
    let shared_local = local();
    let alice =
        CachingStore::new(Arc::clone(&shared_local), Arc::clone(&upstream), Arc::clone(&gate), Who::named("alice"));
    assert_eq!(alice.read(&k).unwrap(), Some(b"secret".to_vec()));
    let anon = CachingStore::new(shared_local, upstream, gate, Who::anonymous());
    assert_eq!(anon.read(&k), Err(Error::Denied));
}
