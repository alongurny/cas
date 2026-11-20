# Changelog

What changed in cas, in prose, newest first. Every crate is still at 0.1.0
and nothing has been released yet. The first release will take the text
under "Unreleased" as its changelog (`cas publish --changelog`), and every
later one says whether it is a breaking, feature, patch or security change
from the one before.

## Unreleased

### The store

A store keeps blobs named by their hash, with the hash name before the
digits (`blake3:…`), and variables that change only by compare-and-set and
hold a view, `<format> <key>`. There are memory and disk backends with one
conformance suite. The disk store compresses with zstd, cuts big blobs into
content-defined chunks, writes batches to pack files with deltas up to 50
deep, maps its pack indexes and compacts packs geometrically. gc marks from
the variables on every core, repacks incrementally and keeps a delta whose
base it removes. fsck reads every blob a history reaches.

### The wire

Stores talk HTTPS and nothing else; plain HTTP exists only behind the
`bench-insecure` feature, for benchmarks. Requests carry batches of keys,
big blobs travel as the chunks the other side lacks, and clone and push send
blobs in their stored forms. Access is bearer tokens with grants kept in a
meta-store, and `cas serve` bounds request bodies.

### Formats

A format is a type, and one compiler derives its parser, its printer and its
circuit; a blob is valid exactly when it parses and prints back to the same
bytes. The formats are directory (raw, symlink, directory and exec
children), commit, annotation, lines, runs, release, note, author and tag. A
name is a short run of bytes from a fixed set that allows `+`; a name that
does not fit is refused, never escaped or renamed.

### Histories, commits and releases

A history is a variable that holds a commit. A commit carries its version,
author, time, notes and its parents with each step's annotation. There are
no commit messages: notes sit on the lines a step changed, and the diff is
recomputed rather than stored. Merge is three-way by path with diff3 inside
text files; rebase and fast-forward are preferred. A release is a commit's
version under a semantic version, with a typed change from the release
before and a prose changelog, in a release history that names no commit,
author or note, so it can be public while the work stays private.

### The cas program

Commands for the whole cycle: commit, status, reset, clean, stash, watch,
checkout and sparse checkout on a directory; log, ls, diff, notes, summary,
path-log, last-modified and merge-base to read a history; merge, rebase,
push, pull, publish, compatible and import-git to join and share; gc, fsck,
objects, show, annotated, serve and access for the store; and the forge's
own commands (accounts, projects, proposals, issues, releases, mirrors).
One command table prints the help, the manual page,
docs/commands.md and completions for bash, fish and zsh. The Nix flake
installs both programs, the manual page and the completions.

### The forge

cas-forge keeps all its state (proposals, reviews, issues, accounts,
events) in the store as formats, with no other database. It speaks git, so
git clients clone and push to it; release tags are git tags. A SvelteKit
frontend that fits a phone sits over its API, and `migrate-gitea` moves
Gitea repositories into it.

### Git

`cas import-git` imports the commits `HEAD` reaches as a history of commits,
and commits convert back to git. Git's annotated tags are kept.

### Proofs

A manifest names what one proof covers for a commit: its blobs, its parents
and the blobs it opens. `cas prove` and `cas verify` make and check proofs
that a commit's blobs are valid, and `cas serve --prove` proves on the
server. The prover is the crate in `prove/`, built against Flock.

### Lean

`cas lean` imports Lean declarations into the store and checks them as
derivations, following Lean's kernel: inductive blocks (nested ones through
the mutual translation), recursors, projections, eta, quotients and
literals. Every inductive block of Init checks. `cas-lean` (in `lean-cli/`)
proves a Lean theorem's dependency cone with one succinct proof and verifies
it; a bundle's statement is the declarations it covers.
