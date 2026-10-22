# cas-core

Stages 2 and 3 of the CAS project: the core store from the Stage 1 spec, and the wire protocol in [spec/wire.md](spec/wire.md).

- `CoreStore` runs every op in spec order: gate, key syntax, store policy, content checks, state.
- Backends: `mem::MemBackend` (volatile, for tests and caches) and `disk::DiskBackend` (durable; temp file + fsync + rename, a lock file around tag CAS).
- `algo::REGISTRY` maps names to hash algorithms (`blake3`, `sha256`, `sha512`, `sha3-256`, and `sha1`, `md5` marked broken). Which ones a store accepts is `Policy`.
- `Gate` decides allow/deny from `(who, op, args)` only; `AllowAll` is the default.
- `server::Server` serves a store over HTTP; `client::HttpStore` is a `Store` that talks to one. `cache::CachingStore` answers blob reads locally and forwards everything else, running the same gate.

## Conformance suite

`conformance` (feature `conformance`) holds one check per spec 9.x assertion. `conformance_tests!(harness)` turns them into tests for any harness that builds a fresh store from a policy and a gate; `tests/conformance.rs` runs it against five harnesses: the memory and disk backends directly, over HTTP, through a cache, and through a cache over HTTP.

```
cargo test --features conformance
```
