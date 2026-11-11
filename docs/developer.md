# Working on cas

This page is for someone changing cas. The behaviour itself is specified in
[../spec/](../spec/); this page says where things are and how to add to them.

## Build, test, check

```sh
cargo build --release --workspace     # target/release/cas and cas-forge
scripts/check                         # formatting, lints, every test, kept files
```

`scripts/check` is what must pass before a push. It runs `cargo fmt --check`,
`cargo clippy -D warnings` and `cargo test` on the whole workspace with every
feature. `scripts/check --renew` first rewrites the files the tool prints
(below). To have git run the check before each push:

```sh
git config core.hooksPath scripts/hooks
```

With Nix, `nix develop` gives the toolchain, `nix build` the package and
`nix flake check` the same checks in a clean build.

The tests need `git` on the path (the git import and the forge's git
endpoint are tested against the real program) and bind servers to ports on
localhost that the system picks.

## The workspace

| Crate | Directory | What it is |
| --- | --- | --- |
| `cas-core` | `src/` | The store: blobs and variables, four operations, backends, the HTTPS wire |
| `cas-version` | `version/` | Everything built from blobs: formats, versions, commits, notes, merge, sync, gc, fsck |
| `cas-cli` | `cli/` | The `cas` program |
| `cas-forge` | `forge/` | The `cas-forge` server: projects, proposals, issues, a git endpoint, the web pages (`forge/web/`) |

A crate uses only the ones above it in this table. The core knows nothing of
formats: to it a blob is bytes and a variable is a name that holds a view.

### cas-core

- `store.rs`: the `Store` trait (read, write, remove, list) and `CoreStore`,
  which runs every operation in the order the specification gives: gate, key
  syntax, policy, content checks, state.
- `disk.rs`, `pack.rs`, `chunk.rs`: the directory backend. Blobs are
  compressed, large ones are cut into shared chunks, and many small ones live
  in packs. None of this is visible in a key: a key hashes the whole value.
- `mem.rs`: a backend in memory, for tests and caches.
- `server.rs`, `client.rs`, `wire.rs`: the store over HTTPS
  ([../spec/wire.md](../spec/wire.md)). `cache.rs` answers blob reads locally
  in front of another store.
- `algo.rs`: the hash algorithms by name. A key is always the name of its
  hash, a colon and the digits, and that is also how it is shown.
- `conformance.rs` (feature `conformance`): one check for each assertion of
  the core specification, run against every backend and transport by
  `tests/conformance.rs`.

### cas-version

- `formats.rs`: the list of formats. A format's id is the key of its format
  object, which names its specification ([../spec/metaformat.md](../spec/metaformat.md)).
- `typed.rs`: the formats given as types ([../spec/types.md](../spec/types.md)):
  `directory`, `commit`, `annotation`, `lines`, `runs`, `release` and the
  texts. One generic parser and printer serve all of them.
- `dir.rs`, `tree.rs`, `fs.rs`, `index.rs`, `watch.rs`: directories, the walk
  and diff of versions, and moving a version to and from a real directory.
- `commits.rs`, `releases.rs`: histories of commits and release histories.
- `annotated.rs`, `stepnotes.rs`: line diffs and the notes on them.
- `merge.rs`, `sync.rs`, `gc.rs`, `fsck.rs`, `access.rs`: what their names say,
  each with a specification of the same name.
- `history.rs`, `htree.rs`, `graph.rs`, `ancestry.rs`, `derived.rs`: the
  earlier history format, which the forge still keeps. New work goes on
  commits; these go when the forge has moved over.
- `git.rs`, `gitobj.rs`, `git_notes.rs`: reading git repositories.

### cas-cli

- `help.rs` holds the one table of commands and options. The help texts, the
  manual page, [commands.md](commands.md) and the completions are printed
  from it.
- `commits.rs` runs the commands on histories of commits and release
  histories, `main.rs` the commands on a store and on the earlier format, and
  `forge.rs` the commands that talk to a forge.

## The rule every format follows

A blob is valid in a format exactly when parsing it succeeds and printing
the parsed value gives the same bytes back. So every value has one encoding,
and a check is never a second, looser reader: validation goes through the
format's own parser and printer. Tests of a format parse and print real
blobs and damaged copies of them; do the same for a new one.

## Adding a format

For a format that is a type ([../spec/types.md](../spec/types.md)):

1. Write `spec/formats/<alias>.md`. Give the type first, then what each field
   means. The file's bytes decide the format's id, so a later edit of the
   file is a new format.
2. Add the `Type` value to `TYPES` in `version/src/typed.rs`, and a line to
   `FORMATS` in `version/src/formats.rs` with its check.
3. If blobs of the format hold keys, `sync::children_of` must list them, or
   push, gc and fsck will not follow them. The typed formats get this from
   the type.
4. Add blobs of it to `version/tests/typed.rs`: valid ones, and damaged ones
   that must be refused.

A change to an existing format is a new version of it: keep the old
specification file and list it as an earlier version (`format_since`), so
that stored blobs stay readable.

## Adding a command

1. Add it to `COMMANDS` in `cli/src/help.rs`: its forms, one line, the longer
   text, its options. New options go in `OPTIONS`, and their parsing in
   `parse` in `cli/src/main.rs`. Placeholders are lower case in angle
   brackets.
2. Run it from `cli/src/commits.rs` (and list it in `PORTED`, or `NO_HISTORY`
   when it needs no history).
3. Test it end to end in `cli/tests/cli.rs`: the tests run the built program.
4. `scripts/check --renew` rewrites `docs/commands.md`, `docs/cas.1` and
   `completions/`. A test fails while they differ from what the tool prints.

Errors are a line or two on standard error, without the word "error" and
without a full stop. One that comes from how the command was written ends
with the usage of the command, which makes the exit status 2; any other
failure is 1.

Every hash shown to a person has the name of its hash directly before it,
also when it is shortened: use `short` in `cli/src/main.rs`.

## Benchmarks

`bench/git-vs-cas.sh <git repository>` times cas against git on the same
repository, operation by operation; [../spec/bench-git.md](../spec/bench-git.md)
has the method and results, and [../spec/performance.md](../spec/performance.md)
what was done about them. Plain HTTP exists only for these runs, behind the
feature `bench-insecure`.

## Proofs

The typed formats are laid out so that validity is cheap to prove: 64-byte
blocks, fixed places, one encoding. The circuits that prove them are
generated from the same `Type` values. That work is not in this repository
yet.
