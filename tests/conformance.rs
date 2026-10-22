use cas_core::conformance::Harness;
use cas_core::disk::DiskBackend;
use cas_core::mem::MemBackend;
use cas_core::{CoreStore, Gate, Policy, Store, Who};
use std::sync::Arc;

fn mem(policy: Policy, gate: Box<dyn Gate>) -> Arc<dyn Store> {
    Arc::new(CoreStore::with_gate(MemBackend::new(), gate, policy).session(Who::anonymous()))
}

fn disk(policy: Policy, gate: Box<dyn Gate>) -> Arc<dyn Store> {
    // Leaked so the directory outlives the store for the test's duration.
    let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
    Arc::new(CoreStore::with_gate(DiskBackend::open(dir.path()).unwrap(), gate, policy).session(Who::anonymous()))
}

mod mem_backend {
    cas_core::conformance_tests!(super::mem as fn(_, _) -> _);
}

mod disk_backend {
    cas_core::conformance_tests!(super::disk as fn(_, _) -> _);
}

#[test]
fn disk_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let k = cas_core::algo::lookup("blake3").unwrap().key(b"durable");
    {
        let s = CoreStore::new(DiskBackend::open(dir.path()).unwrap(), cas_core::AllowAll, Policy::permissive()).session(Who::anonymous());
        s.write(&k, b"durable", &cas_core::Expect::Absent).unwrap();
        s.write("main", k.as_bytes(), &cas_core::Expect::Absent).unwrap();
    }
    let s = CoreStore::new(DiskBackend::open(dir.path()).unwrap(), cas_core::AllowAll, Policy::permissive()).session(Who::anonymous());
    assert_eq!(s.read("main").unwrap(), Some(k.as_bytes().to_vec()));
    assert_eq!(s.read(&k).unwrap(), Some(b"durable".to_vec()));
    let _: &dyn Harness = &(disk as fn(_, _) -> _);
}
