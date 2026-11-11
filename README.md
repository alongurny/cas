# cas

cas keeps the versions of a directory in a content-addressed store. Every
version, commit and note is a blob named by its hash, and a history is a name
that holds its newest commit. A commit says why through notes on its change,
not through a message, and a release carries a semantic version and a
changelog.

```sh
$ cas commit site
blake3:e191620fdc5641ab9b903af119497904a35b11d2ff5789454d99ff149bddb57b
$ cas log
blake3:e191620fdc56  []  2026-11-11 20:02  Dana
```

## Install

```sh
cargo install --path cli                 # the cas program
cargo install --path forge               # the cas-forge server, if wanted
cas completions fish > ~/.config/fish/completions/cas.fish
```

`cas help completions` has the lines for bash and zsh. With Nix,
`nix profile install .` installs both programs, the manual page and the
completions of all three shells.

## Documentation

- [docs/guide.md](docs/guide.md): using cas, from the first commit to a release.
- [docs/commands.md](docs/commands.md): every command and option. The same
  text is `cas help <command>` and the manual page [docs/cas.1](docs/cas.1).
- [docs/proofs.md](docs/proofs.md): proofs that the blobs of a commit are valid,
  and how to build the prover.
- [docs/developer.md](docs/developer.md): the layout of the code, how to add
  a format or a command, and the checks before a push.
- [spec/](spec/): what the store, the wire, the formats and the forge are
  required to do.

## Terms

A **blob** is immutable bytes named by their hash. A hash is written as the
name of its hash, a colon and its digits (`blake3:e191...`), in the store and
wherever it is shown. A **variable** is a mutable value named by whoever chose
the name, changed only by compare-and-set. A **view** is `<format> <key>`: a
blob together with the format to read it in, where a format is itself named
by the key of its description. A variable holds only a view, so every
variable is a root that push, gc and fsck can follow.

A **history** is a variable that holds a view of a commit. Formats are
checked by one rule: a blob is valid exactly when parsing it succeeds and
printing the result gives the same bytes.

## Layout

| Crate | Directory | What it is |
| --- | --- | --- |
| `cas-core` | `src/` | The store, its backends and the HTTPS wire |
| `cas-version` | `version/` | Formats, versions, commits, notes, merge, sync, gc, fsck |
| `cas-cli` | `cli/` | The `cas` program |
| `cas-forge` | `forge/` | The forge server and its web pages |

```sh
cargo build --release --workspace
scripts/check                            # formatting, lints, every test
```
