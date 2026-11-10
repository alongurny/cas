# cas

The CAS project's Rust workspace.

Terms: a **blob** is immutable bytes named by their hash; a **variable** is a mutable value named by
whoever chose the name, changed only by compare-and-set (the Stage 1 core spec calls these tags); a
**view** is `<format> <key>`, a blob together with the format to read it in. A **tag** is a label
inside a history.

A variable holds only a view (amending the Stage 1 spec, where tag values were any bytes): a write of
anything else is `invalid` (`view`). Only the syntax is checked, two blob keys and one space; the
blobs need not exist. So every variable is a root the tooling can follow.

- `cas-core` (this directory), Stages 2 and 3: the core store from the Stage 1 spec, and the wire protocol in [spec/wire.md](spec/wire.md).
- `cas-version` ([version/](version)), Stage 4: the version format in [spec/version.md](spec/version.md). It is a tooling layer of ordinary blobs that the core does not know about. Every view is `<format> <key>`, and a format's id is the key of its format object in the single metaformat ([spec/metaformat.md](spec/metaformat.md)). Formats nest: directory formats hold views. The formats so far are `dir`, `raw`, `exec`, `symlink` and `git-blob` ([spec/formats/](spec/formats)). It provides build, update, lookup, prefix walk (sparse checkout), diff, and import/checkout of a local directory, and it keeps empty directories.
- Histories ([spec/history.md](spec/history.md)), also in `cas-version`. `uni-bin` is a general data format of strings and maps, stored as a canonical binary serialization. A version has no parents; a `history` object holds versions as nodes in an order (a DAG), with notes (an annotated diff) on each step and a changelog on a release's, an opt-in compatible flag (every step breaks unless marked), tags and docs. A variable (the store's mutable, compare-and-set names) points at the current history. The `history` module provides commit, tags, log, merge bases, compatibility and extension checks. Branches are not first-class: private histories, rebase and publish are the workflow.
- Stage 6, derived data ([spec/derived.md](spec/derived.md)), module `derived`. `derive(F, X)` caches f(X) under the variable `derived/F/X`, where F is the key of a function object. The first function is `summary`, computed recursively, so it is incremental. `path_log` lists the nodes of a history that changed a path.
- Stage 7, sync ([spec/sync.md](spec/sync.md)), module `sync`. `copy` writes what a destination lacks, children before parents, and stops at the first blob already present. `push` creates a history name or fast-forwards it to an extension, and refuses one that has diverged. Only the four core ops are used, so it works against any store, cache or server.
- Merge ([spec/merge.md](spec/merge.md)), module `merge`: three-way at path level with diff3 inside text files both sides changed. On histories: `rebase` replays private nodes onto a public tip, `publish` adds a private tip to a public history as one node, and `merge_nodes` joins two tips (a virtual base after a criss-cross).
- Reachability and collection ([spec/gc.md](spec/gc.md)), module `gc`. `mark` walks formats from every name; `collect` removes a blob only after two runs in a row found it unreachable, so writers have a run's time to link what they wrote. Deleting content that versions still use is out of scope.
- The outer protocol ([spec/access.md](spec/access.md)), module `access`: bearer tokens (stored only as hashes) name the caller, and an allow-list of grants such as `read blobs` or `write var:main` decides each op. Users and grants are one `uni-bin` document in a separate meta-store, re-read whenever its variable moves.
- `cas-cli` ([cli/](cli)), a prototype command line, `cas`. It covers commit, log, ls, diff, checkout, summary, path-log, tag, compatible, show, push, pull, rebase, publish, merge, gc, access and serve, and import-git ([spec/git-import.md](spec/git-import.md), with dogfooding measurements on the Thesis repository), over a store directory (default `.cas`) or an `https://` server (`--token-file` for one under access control, `--ca` to trust a private CA); `cas serve` needs `--cert` and `--key`, since HTTPS is the only protocol. `cli/tests/cli.rs` runs all of them end to end.

```
cargo run -p cas-cli -- commit some/dir
cargo run -p cas-cli -- log
```

## cas-core

- `CoreStore` runs every op in spec order: gate, key syntax, store policy, content checks, state.
- Backends: `mem::MemBackend` (volatile, for tests and caches) and `disk::DiskBackend` (durable; temp file + fsync + rename, a lock file around variable CAS). A new disk store (marker file `format` = `2`) compresses blobs with zstd and stores blobs over 64 KiB as content-defined chunks (FastCDC, `chunk.rs`) shared between blobs; a store made before that stays raw. This is storage only: keys hash the whole value, and reads return it whole.
- `algo::REGISTRY` maps names to hash algorithms (`blake3`, `sha256`, `sha512`, `sha3-256`, and `sha1`, `md5` marked broken). Which ones a store accepts is `Policy`.
- `Gate` decides allow/deny from `(who, op, args)` only; `AllowAll` is the default.
- `server::Server` serves a store over HTTPS only (rustls; spec/wire.md, Transport); `client::HttpStore` is a `Store` that talks to one. `cache::CachingStore` answers blob reads locally and forwards everything else, running the same gate.

## Conformance suite

`conformance` (feature `conformance`) holds one check per spec 9.x assertion. `conformance_tests!(harness)` turns them into tests for any harness that builds a fresh store from a policy and a gate; `tests/conformance.rs` runs it against five harnesses: the memory and disk backends directly, over HTTPS, through a cache, and through a cache over HTTPS. Feature `test-tls` adds `server::self_signed` for such tests.

```
cargo test --workspace --all-features
```

That runs both crates. The version format's suite is `version/tests/conformance.rs`.
