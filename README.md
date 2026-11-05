# cas

The CAS project's Rust workspace.

- `cas-core` (this directory), Stages 2 and 3: the core store from the Stage 1 spec, and the wire protocol in [spec/wire.md](spec/wire.md).
- `cas-version` ([version/](version)), Stage 4: the version format in [spec/version.md](spec/version.md). It is a tooling layer of ordinary blobs that the core does not know about. Every reference is `<format> <key>`, and a format's id is the key of its format object in the single metaformat ([spec/metaformat.md](spec/metaformat.md)). Formats nest: directory formats hold references. The formats so far are `dir`, `raw`, `exec`, `symlink` and `git-blob` ([spec/formats/](spec/formats)). It provides build, update, lookup, prefix walk (sparse checkout), diff, and import/checkout of a local directory, and it keeps empty directories.
- Stage 5, history records ([spec/history.md](spec/history.md)), also in `cas-version`. `uni` is a general data format of strings and maps, stored as a canonical binary serialization. `record` holds a version, its parents and metadata. The `history` module provides commit (moving a branch tag by compare-and-swap), log, and merge bases.

## cas-core

- `CoreStore` runs every op in spec order: gate, key syntax, store policy, content checks, state.
- Backends: `mem::MemBackend` (volatile, for tests and caches) and `disk::DiskBackend` (durable; temp file + fsync + rename, a lock file around tag CAS).
- `algo::REGISTRY` maps names to hash algorithms (`blake3`, `sha256`, `sha512`, `sha3-256`, and `sha1`, `md5` marked broken). Which ones a store accepts is `Policy`.
- `Gate` decides allow/deny from `(who, op, args)` only; `AllowAll` is the default.
- `server::Server` serves a store over HTTP; `client::HttpStore` is a `Store` that talks to one. `cache::CachingStore` answers blob reads locally and forwards everything else, running the same gate.

## Conformance suite

`conformance` (feature `conformance`) holds one check per spec 9.x assertion. `conformance_tests!(harness)` turns them into tests for any harness that builds a fresh store from a policy and a gate; `tests/conformance.rs` runs it against five harnesses: the memory and disk backends directly, over HTTP, through a cache, and through a cache over HTTP.

```
cargo test --workspace --all-features
```

That runs both crates. The version format's suite is `version/tests/conformance.rs`.
