# Proofs of validity

A store holds blobs that say what they are: a commit, a directory, the notes
of a step. Someone who fetches a history normally checks each of them by
reading it. A proof lets them skip that: it is a few hundred kilobytes that
say the blobs a commit brought are all valid, without the blobs.

```sh
$ cas prove --all
blake3:5a635f13...  proven, 303180 bytes  8.45s
blake3:4c8105d6...  proven, 374252 bytes  0.23s
2 commits proven: 7 blobs valid
$ cas verify --all
blake3:5a635f13...  verified  8.31s
blake3:4c8105d6...  verified  0.20s
2 commits verified: 7 blobs valid
```

Each line is the key of a commit's manifest (shortened here), oldest commit
first.

## What is proven

Each commit has a **manifest** ([../spec/formats/manifest.md](../spec/formats/manifest.md)):
the blobs it brings that its parents do not have in the same place, each
with its type and its length. `cas manifest` prints it. It lists the commit
itself, its directories, its author, its notes and the line diffs they are
on. It does not list files (any bytes are a valid file) and does not list a
parent commit, which has a manifest of its own.

A **proof** for a manifest says that each blob listed

- has the length listed,
- hashes, with BLAKE3, to the key listed, and
- is valid in the type listed: parsing it succeeds and printing the result
  gives the same bytes.

The manifest is all a verifier needs beside the proof. Both are kept as
derived data ([../spec/derived.md](../spec/derived.md)): the manifest is the
result of the function `manifest` on the commit, and the proof the result of
the function `proof` on the manifest. So they travel and are collected like
any other derived result, and a store need not have them.

## What is not proven

- That the manifest is the right one for its commit. `cas verify` works the
  manifest out again from the commit, which reads the blobs. A client that
  wants to trust a manifest it did not work out needs a proof of that too;
  there is none yet.
- One proof for a whole history. Each commit has its own proof, and
  `--all` checks them one by one.
- Release histories and tags. No circuit checks a semantic version yet.
- Anything about a remote store. `prove` and `verify` run where the store
  directory is.

## The prover

The proofs are made by [Flock](https://github.com/succinctlabs/flock), with
circuits generated from the same type values that parse the formats
(`version/src/typed.rs`). Flock is large, so the prover is a program of its
own, `cas-prove`, in `prove/`, outside the workspace. `cas prove` and
`cas verify` run it; without it they say that it is not installed, and
every other command works.

To build it, put the Flock checkout with the cas circuits (`alon/flock`,
branch `cas`) beside the cas checkout, as `../flock`:

```sh
cd prove
cargo build --release        # prove/target/release/cas-prove
cargo test --release
```

Put `cas-prove` on the path, or in the directory `cas` is in.
`scripts/check` also checks `prove/` when `../flock` is there.

## Cost

Measured on a cloud machine, on the history of a thesis
(4,550 commits, 88,620 blobs listed, 46.5 MB):

- The first proof or check in a run takes about 8.4 seconds to set up.
  `cas prove --all` and `cas verify --all` pay it once for every 256 commits.
- After that a commit takes 0.2 to 0.3 seconds to prove and about 0.2 to
  check.
- A proof is 300 to 400 kilobytes.

Proving is meant for the server that receives a push; a client fetches the
proofs of the commits it relies on and checks those.
