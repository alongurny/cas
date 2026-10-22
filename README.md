# cas-core

Stage 2 of the CAS project: the core store from the Stage 1 spec.

- `CoreStore` runs every op in spec order: gate, key syntax, store policy, content checks, state.
- Backends: `mem::MemBackend` (volatile, for tests and caches) and `disk::DiskBackend` (durable; temp file + fsync + rename, a lock file around tag CAS).
- `algo::REGISTRY` maps names to hash algorithms (`blake3`, `sha256`, `sha512`, `sha3-256`, and `sha1`, `md5` marked broken). Which ones a store accepts is `Policy`.
- `Gate` decides allow/deny from `(who, op, args)` only; `AllowAll` is the default.

## Conformance suite

`conformance` (feature `conformance`) holds one check per spec 9.x assertion. `conformance_tests!(harness)` turns them into tests for any harness that builds a fresh store from a policy and a gate; Stage 3 reuses it over the wire and through a cache.

```
cargo test --features conformance
```
