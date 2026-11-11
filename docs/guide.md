# Using cas

cas keeps the versions of a directory in a content-addressed store. This guide
walks through a working day with it. Every command is explained by
`cas help <command>`, and all of them are listed in [commands.md](commands.md).

## Words

- A **blob** is bytes that never change, named by their hash. A hash is always
  shown with the name of its hash directly before it: `blake3:61abff9f8ae8...`.
  Where a short form is enough, the digits are cut and the name stays:
  `blake3:61abff9f8ae8`.
- A **store** holds blobs and a few names. It is a directory (`.cas` unless
  `--store` or `$CAS_STORE` says otherwise) or an `https://` server.
- A **version** is a directory tree as a blob. A **commit** is a version with
  its author, its time, its parents and its notes.
- A **history** is a name that holds its newest commit, such as `main`. `-H`
  chooses the history; `-n` chooses a commit in it (`latest` unless given).
- A **release** is a version with a tag, in the release history `releases`.

There are no commit messages and no branches. A commit says why through
**notes** on the change itself, and a line of work that is not ready is a
history of its own.

## Install

```sh
cargo install --path cli        # puts cas in ~/.cargo/bin
cas completions fish > ~/.config/fish/completions/cas.fish
```

`cas help completions` has the lines for bash and zsh. With Nix,
`nix profile install .` installs the tool, its manual page and the
completions of all three shells. The manual page is [cas.1](cas.1):
`man docs/cas.1`.

## The first commit

```sh
$ export CAS_AUTHOR=Dana
$ cas commit site
blake3:e191620fdc5641ab9b903af119497904a35b11d2ff5789454d99ff149bddb57b
$ cas log
blake3:e191620fdc56  []  2026-11-11 20:02  Dana
```

`commit` stores the directory `site` as a new commit of `main` and prints its
key. The first commit makes the store, in `.cas`. Each line of `log` is a
commit, its parents in brackets, its time in UTC, its author and the first
line of its note.

## Changing things

```sh
$ cas status site
changed  src/main.rs
$ cas commit site
```

`status` compares the directory with the newest commit and stores nothing.
`reset site` throws the changes away, `clean site` removes files the commit
does not have, and `stash site` sets the changes aside (`stash pop site`
brings them back).

`cas ls`, `cas diff`, `cas summary`, `cas path-log <path>` and
`cas last-modified` read a commit. With `-n` they read an older one:

```sh
$ cas -n blake3:e191620fdc56 ls
$ cas -n e191 checkout old-site        # the start of a key is enough
```

## Saying why: notes

A commit explains itself with notes on the diff. Ask for the diff, add lines
that start with `# ` above what they are about, and commit with it:

```sh
$ cas status site --diff > step.notes
$ $EDITOR step.notes
$ cat step.notes
# call run from main
| diff blake3:b489... blake3:5454... blake3:b489... blake3:df97...
| path src/main.rs
| old blake3:180c... blake3:2d1e...
| new blake3:180c... blake3:ce31...
| @@ -1,1 +1,3 @@
| -fn main() {}
| +fn main() {
# run does the work
| +    run();
| +}
$ cas commit site --notes step.notes
$ cas log
blake3:61abff9f8ae8  [blake3:e191620fdc56]  2026-11-11 20:02  Dana  call run from main
blake3:e191620fdc56  []  2026-11-11 20:02  Dana
```

A note covers the lines below it until the next note; `#` alone ends it. The
note that covers the first line is the note of the whole step and shows in
`log`, one that covers a `| path` line is that path's, and the others are on
the lines of the diff. The lines
that start with `| ` must stay exactly as printed: cas checks that the file is
the diff of what is being committed.

`cas diff --annotated` prints a commit's diff with its notes, and
`cas notes [<path>]` the notes alone.

To note a whole file as it is, instead of a change, write each of its lines
as `| line` with `# note` lines between, and pass
`--annotate <path>=<file>` to `commit`.

## More than one line of work

A side line is another history. Start it from `main`, work on it, and bring
it back:

```sh
$ cas -H topic pull .cas --as main     # topic starts where main is
$ cas -H topic commit site
$ cas merge topic                      # on main: one commit with both
$ cas -H topic rebase main             # or: replay topic's commits onto main
```

`merge` merges paths three ways, and the lines of a text file that both sides
changed. When it finds a conflict it lists the conflicts and records nothing.

## Other stores

```sh
$ cas push /backup/site.cas
$ cas pull https://cas.example.org --token-file ~/.cas-token
```

`push` sends what the other store lacks and moves the history there forward.
It never overwrites: if the other side has commits this one lacks, it refuses
and says so. `--as <name>` uses another name on the other side.

`cas serve <addr> --cert <file> --key <file>` serves a store over HTTPS, the
only protocol. Who may do what is the access policy, which `cas access`
edits; see `cas help access` and [../spec/access.md](../spec/access.md).

## Releases

```sh
$ cas publish --tag 1.0.0
1.0.0 blake3:e655cc475682
$ cas publish --tag 1.0.1 --change patch --changelog notes-1.0.1.txt
$ cas -H releases log
1.0.1  blake3:e77fbf1fb464
  patch from 1.0.0: Fix the heading.
1.0.0  blake3:e655cc475682
$ cas -H releases compatible 1.0.0 1.0.1
compatible
$ cas -H releases -n 1.0.0 checkout site-1.0.0
```

A release is the version of a commit under a semantic version. The first has
no change; each later one says what kind of change it is from the one before
(`breaking`, `feature`, `patch` or `security`) and carries a changelog. No
commit, author or note reaches the release history, so it can be public
while the work stays private.

## From git

```sh
$ cas import-git path/to/repo
```

imports the commits that `HEAD` reaches as a new history. A commit's message
becomes the note of its step.

## Looking after a store

- `cas fsck` reads every blob a history reaches and checks that it is there,
  that its hash is right and that it is valid in its format.
- `cas gc` removes blobs nothing reaches. A blob goes only after two runs in
  a row found it unreachable.
- `cas prove` makes a proof that what a commit brought is valid, and
  `cas verify` checks it; `--all` does the whole history. They need the
  prover, a program of its own: see [proofs.md](proofs.md).

## When something goes wrong

An error is one or two lines on standard error. The exit status is 0 when the
command did what it says, 1 when it could not, and 2 when the command was not
written correctly; then the error shows how it is written.
