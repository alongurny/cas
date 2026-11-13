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
on. It does not list files (any bytes are a valid file). It names the
parent commits, which have manifests of their own, and the blobs of the
parents that the new blobs were compared with ("opened").

A **proof** for a manifest says that each blob listed

- has the length listed,
- hashes, with BLAKE3, to the key listed, and
- is valid in the type listed: parsing it succeeds and printing the result
  gives the same bytes;

and that each key a new blob holds (not a file, not a parent) is the key of
a new blob or is held by an opened blob or a parent, in a field of the same
type, and each opened blob is held by an opened blob or a parent. So if the
parents are valid, everything the commit reaches is: `cas verify` checks
the parents' proofs as well, back to the first commit.

The manifest is all a verifier needs beside the proof. Both are kept as
derived data ([../spec/derived.md](../spec/derived.md)): the manifest is the
result of the function `manifest` on the commit, and the proof the result of
the function `proof` on the manifest. So they travel and are collected like
any other derived result, and a store need not have them.

## Releases

A release history is proven the same way, release by release:

```sh
$ cas -H releases prove --all
$ cas -H releases verify --all
$ cas -H releases manifest -n 1.1.0
```

A release's manifest (the function `release-manifest`,
[../spec/functions/release-manifest.md](../spec/functions/release-manifest.md))
lists the release, its tag (proven a semantic version), its changelogs
(proven prose) and the part of its version that the releases it follows do
not have in the same place. So a release history can be checked without the
commits its versions came from.

## What is not proven

- That the manifest is exactly the one its commit gives. It need not be:
  the proof shows every key the new blobs hold is found, so a verifier that
  holds a manifest listing the commit, its proof and its parents' proofs
  knows everything the commit reaches is valid. `cas verify` still works the
  manifest out from the commit, which reads the blobs.
- One proof for a whole history. Each commit has its own proof, and
  `--all` checks them one by one.
- Nothing is proven in a remote store from outside: `prove` and `verify` run
  where the store directory is. A server proves its own store (below).

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

## On a server

Proving is the part of the store that receives a push, and a client keeps
only the proofs it asks for:

```sh
server$ cas serve 0.0.0.0:443 --cert cert.pem --key key.pem --prove
client$ cas push https://cas.example.org
client$ cas verify --all --from https://cas.example.org
```

With `--prove` the server proves each history of commits whenever it moves,
as `cas prove --all` does, and says so on standard error. `verify --from`
fetches the proofs that are not in the local store from the other one, keeps
them and checks them; what it fetches is not trusted, only checked. A push
is accepted before its proof exists, so for a moment after a push the server
has none.
