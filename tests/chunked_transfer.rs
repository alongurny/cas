//! A big blob goes over the wire as the chunks the server lacks
//! (spec/wire.md, "Chunked transfer").

use cas_core::client::HttpStore;
use cas_core::disk::DiskBackend;
use cas_core::mem::MemBackend;
use cas_core::server::Server;
use cas_core::{CoreStore, Error, Expect, Gate, Op, Policy, Store, Who, algo};
use std::sync::{Arc, Mutex};

/// Allows everything except what `deny` matches, and records the bytes of
/// every write it sees.
#[derive(Default)]
struct Watch {
    writes: Mutex<Vec<(String, usize)>>,
    deny_writes: bool,
}

struct W(Arc<Watch>);

impl Gate for W {
    fn allow(&self, _: &Who, op: &Op<'_>) -> bool {
        if let Op::Write { key, value, .. } = op {
            self.0.writes.lock().unwrap().push((key.to_string(), value.len()));
            return !self.0.deny_writes;
        }
        true
    }
}

fn data(seed: u64, n: usize) -> Vec<u8> {
    let mut x = seed;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

fn serve(backend: impl cas_core::Backend + 'static, watch: Arc<Watch>) -> (Server, HttpStore) {
    let core = CoreStore::new(backend, W(watch), Policy::permissive());
    let (tls, ca) = cas_core::server::self_signed();
    let server =
        Server::start(core, "127.0.0.1:0", 2, Arc::new(|_: &cas_core::server::Request| Who::anonymous()), tls).unwrap();
    let client = HttpStore::new(server.url()).unwrap().trusting(&ca).unwrap();
    (server, client)
}

fn key(v: &[u8]) -> String {
    algo::lookup("blake3").unwrap().key(v)
}

#[test]
fn an_edit_sends_only_new_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let watch = Arc::new(Watch::default());
    let (_server, client) = serve(DiskBackend::open(dir.path()).unwrap(), Arc::clone(&watch));
    let one = data(1, 1 << 20);
    let mut two = one.clone();
    two.splice(500_000..500_000, b"an edit".iter().copied());

    client.write(&key(&one), &one, &Expect::Absent).unwrap();
    watch.writes.lock().unwrap().clear();
    client.write(&key(&two), &two, &Expect::Absent).unwrap();
    assert_eq!(client.read(&key(&two)).unwrap().unwrap(), two);
    assert_eq!(client.read(&key(&one)).unwrap().unwrap(), one);

    let writes = watch.writes.lock().unwrap();
    // Chunk uploads, then the assembled write of the whole blob.
    let (last, chunks) = writes.split_last().unwrap();
    assert_eq!(*last, (key(&two), two.len()));
    let uploaded: usize = chunks.iter().filter(|(_, n)| *n > 0).map(|(_, n)| n).sum();
    assert!(uploaded > 0 && uploaded < 200_000, "sent {uploaded} bytes of chunks for a 7-byte edit");
}

#[test]
fn a_server_without_chunks_takes_the_whole_value() {
    let watch = Arc::new(Watch::default());
    let (_server, client) = serve(MemBackend::new(), Arc::clone(&watch));
    let v = data(2, 300_000);
    client.write(&key(&v), &v, &Expect::Absent).unwrap();
    assert_eq!(client.read(&key(&v)).unwrap().unwrap(), v);
    assert_eq!(*watch.writes.lock().unwrap(), [(key(&v), v.len())]);
}

#[test]
fn chunks_are_gated_like_blob_writes() {
    let dir = tempfile::tempdir().unwrap();
    let watch = Arc::new(Watch { deny_writes: true, ..Watch::default() });
    let (_server, client) = serve(DiskBackend::open(dir.path()).unwrap(), watch);
    let v = data(3, 300_000);
    assert!(matches!(client.write(&key(&v), &v, &Expect::Absent), Err(Error::Denied)));
    assert_eq!(std::fs::read_dir(dir.path().join("chunks")).unwrap().count(), 0);
}

#[test]
fn a_wrong_chunk_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (_server, client) = serve(DiskBackend::open(dir.path()).unwrap(), Arc::new(Watch::default()));
    let id = [0u8; 32];
    assert_eq!(client.put_chunk(&id, b"not that"), Err(Error::Invalid(cas_core::Invalid::HashMismatch)));
}
