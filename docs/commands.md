# cas commands

This page is printed by `cas help --markdown`; a test keeps it equal to the tool.

```
cas [<option>...] <command> [<argument>...]
```

## Options of every command

- `--store <dir>|<url>`: the store: a directory (default $CAS_STORE, else .cas) or an https URL
- `--token-file <file>`: the file holding the token for a store under access control
- `--ca <file>`: trust the CA certificates in <file> instead of the public ones
- `--meta <dir>`: the store holding the access policy (default .cas-meta)
- `-j <threads>`: the most threads a command uses (default: one for each core)
- `-H <history>`: the history to work on (default main)
- `-n <commit>`: a commit: latest, a key or its start; a tag on a release history (default latest)
- `--help`: explain the command instead of running it (also -h)
- `--version`: print the version of cas

## Work on a directory

### commit

```
cas commit <dir> [--author <name>] [--notes <file>] [--annotate <path>=<file>]...
```

Add <dir> as a new commit after the tip.

Prints the key of the new commit. A first commit starts the history.

There is no commit message. A commit says why through notes: --notes takes the diff that `status <dir> --diff` prints with `# note` lines added, on the whole step, on a path or on a line; --annotate takes one file as it is, each line as `| line`, with `# note` lines before the lines they are about.

<dir>/.cas-index caches file sizes and times, so files that did not change are not read again.

- `--author <name>`: who makes the commit (default $CAS_AUTHOR, else $USER)
- `--notes <file>`: notes for the step: what `status <dir> --diff` prints, with `# note` lines added
- `--annotate <path>=<file>`: notes on the file <path> as it is: its lines as `| line`, with `# note` lines

### status

```
cas status <dir>
cas status <dir> --diff
```

How <dir> differs from the commit.

Each changed path is printed as added, deleted or changed. Nothing is stored.

With --diff, prints the line diff from the commit to <dir>, which is what `commit --notes` takes after notes are added. This stores the blobs of <dir>'s files.

- `--diff`: print the line diff, ready to add notes to

### reset

```
cas reset <dir>
```

Make <dir> the commit's version again.

Changes in <dir> are lost.

### clean

```
cas clean <dir>
```

Remove from <dir> the paths the commit does not have.

### stash

```
cas stash <dir>
cas stash pop <dir>
```

Set the changes in <dir> aside, or bring them back.

`stash <dir>` keeps <dir> as a commit in the history stash/<history> and resets <dir>. `stash pop <dir>` puts the newest one back into <dir> and drops it; <dir> must have no changes of its own.

- `--author <name>`: who makes the commit (default $CAS_AUTHOR, else $USER)

### watch

```
cas watch <dir>
```

Follow the changes in <dir> until stopped.

While it runs, commit and status look only at what changed (inotify), which matters in a large directory.

### checkout

```
cas checkout <dest> [<prefix>]
```

Write the commit's version, or the part under <prefix>, to <dest>.

### sparse

```
cas sparse <dest> <path>...
```

Write only the named files and directories of the commit to <dest>.

A later commit of <dest> keeps everything that was not checked out.

## Read a history

### log

```
cas log
```

The commits, newest first.

Each line is the commit, its parents in brackets, its time (UTC), its author and the first line of its note. On a release history each line is a tag and its release, followed by what it changes.

### ls

```
cas ls [<prefix>]
```

The paths of the commit's version.

### diff

```
cas diff
cas diff --annotated
```

What the commit changed.

Prints each changed path with its format before and after, from each parent; a first commit shows every path it has as new. With --annotated, prints the line diff with the notes of the step.

- `--annotated`: print the line diff with the notes of the step

### notes

```
cas notes [<path>]
```

The notes of the commit.

Without <path>, the noted paths. With <path>, its note and the file's lines with the note on each, in the form `commit --annotate` takes.

### manifest

```
cas manifest
```

What a proof of the commit covers.

Lists each blob the commit brings that is not a file: its type, its key and its length. A proof for this list says that each of them is valid. The last line gives the totals and the key of the manifest, which is what a proof is named by. In a release history (-H releases) it is the manifest of the release: the release, its tag, its changelogs and the part of its version the releases before it do not have.

### prove

```
cas prove [--all] [--quiet]
```

Make a proof that what the commit brings is valid.

The proof covers the manifest of the commit (`cas manifest`) and is kept in the store beside it. With --all, every commit the commit reaches that has no proof yet gets one, oldest first. `serve --prove` runs this whenever a history moves. In a release history it proves releases.

Proving is done by the program cas-prove, built from prove/ (docs/proofs.md).

- `--all`: every commit the commit reaches, not only the commit itself
- `--quiet`: print the outcome only, not a line for each proof

### verify

```
cas verify [--all] [--quiet] [--from <store>]
```

Check the proof of the commit.

Checks the proof kept for the manifest of the commit, or with --all of every commit it reaches. A proof is checked against the manifest alone: no listed blob is read. With --from, a proof that is not here is first fetched from that store and kept, so a client holds only the proofs it asked for. Fails when a commit has no proof or a proof does not verify. In a release history it checks releases.

- `--all`: every commit the commit reaches, not only the commit itself
- `--quiet`: print the outcome only, not a line for each proof
- `--from <store>`: fetch the proofs that are not here from this store

### summary

```
cas summary
```

How many directories and files the commit's version has.

### path-log

```
cas path-log <path>
```

The commits that changed <path>.

### last-modified

```
cas last-modified [<prefix>]
```

Each entry under <prefix> with the commit that last changed it.

### merge-base

```
cas merge-base <a> <b>
```

The best common ancestor of commits <a> and <b>.

## Join and share histories

### merge

```
cas merge <other>
```

Merge history <other> into this one as one commit.

Paths are merged three ways, and the lines of a text file both sides changed. On a conflict nothing is recorded and the conflicts are listed.

- `--author <name>`: who makes the commit (default $CAS_AUTHOR, else $USER)

### rebase

```
cas rebase <onto>
```

Replay this history's own commits onto history <onto>.

### push

```
cas push <store> [--as <name>]
```

Send the history to another store.

Only what the other store lacks is sent. The history there is created, or moved forward when this one extends it; one that has diverged is refused.

- `--as <name>`: the name of the history on the other side, or of the release history

### pull

```
cas pull <store> [--as <name>]
```

Fetch the history from another store.

The same rules as push, the other way.

- `--as <name>`: the name of the history on the other side, or of the release history

### publish

```
cas publish --tag <version> [--change <kind> --changelog <file>] [--as <name>]
```

Add the commit's version to the release history as a release.

The release history is `releases`, or --as <name>. A first release has no change. A later one says its change from the release before it: its kind and a changelog.

- `--tag <version>`: the semantic version of the release, such as 1.4.0
- `--change <kind>`: what the release changes since the one before: breaking, feature, patch or security
- `--changelog <file>`: the text that says what the release changes
- `--as <name>`: the name of the history on the other side, or of the release history

### compatible

```
cas -H releases compatible <from> <to>
```

Whether upgrading from release <from> to <to> is safe.

Prints compatible or breaking, from the kinds of change between the two.

### import-git

```
cas import-git <repo> [<rev>] [--as <name>]
```

Import the commits of a git repository as a new history, and its tags as releases.

<rev> is HEAD unless given. Refused when the history exists. A commit's message becomes the note of its step. Each git tag on an imported commit whose name is a semantic version (a leading v aside) becomes a release in `releases`, or --as <name>, oldest first; git does not say what a tag changes, so each is a breaking change, with the tag's message as its changelog. The tags left out are listed with the reason.

- `--as <name>`: the name of the history on the other side, or of the release history

## The store

### gc

```
cas gc [--full]
```

Remove blobs that no history reaches.

A blob is removed only after two runs in a row found it unreachable, so a writer has the time of one run to link what it wrote.

- `--full`: walk the whole store, not only what came since the last full run

### fsck

```
cas fsck
```

Check every reachable blob: readable, valid and complete.

Fails when a blob is missing or not valid in its format, and names it.

### objects

```
cas objects
```

Every blob key in the store.

### show

```
cas show <key>
```

Print a blob.

### annotated

```
cas annotated make <file>
cas annotated show <key>
```

Store or print a text with a note on each line.

### serve

```
cas serve <addr> --cert <file> --key <file> [--open] [--prove] [--max-body <bytes>]
```

Serve the store over HTTPS.

HTTPS is the only protocol. Requests are decided by the access policy in --meta, unless --open serves to anyone. With --prove, each history of commits is proven as it moves, as `prove --all` does, so that those who fetch from it can `verify --from` it.

- `--cert <file>`: the PEM certificate chain to serve with
- `--key <file>`: the PEM private key to serve with
- `--open`: serve to anyone, without an access policy
- `--max-body <bytes>`: the largest request body accepted (default 64 MiB)
- `--prove`: prove each history of commits as it moves (needs cas-prove)

### access

```
cas access show
cas access user <name>
cas access grant <who> '<action> <scope>'
cas access revoke <who> '<action> <scope>'
```

The access policy of a served store.

The policy is in the store --meta names. `access user` adds a user or gives them a new token, printed once. A grant is an action and a scope, such as 'read blobs' or 'write var:main'; <who> is a user or '*' for anyone.

## On a forge

### signup

```
cas signup <name> [--email <e>]
cas login <name>
cas whoami
cas password
```

Make an account, sign in, or change the password.

Passwords are read from standard input. login writes the token to --token-file, or prints it.

### account

```
cas account <name>
cas account edit [--display <d>] [--email <e>]
```

Show or edit an account.

### token

```
cas token list
cas token make <label> [--scope all|read|project:<owner>/<project>] [--days <n>]
cas token revoke <key>
```

The tokens of the account.

### org

```
cas org create <name>
cas org member <org> <user> owner|member|remove
```

Organizations and their members.

### projects

```
cas projects
```

The projects you can read.

### project

```
cas project create <name> [--owner <o>] [--description <d>] [--public]
cas project show
cas project delete --yes
cas project fork [--owner <o>] [--name <n>]
cas project edit [--description <d>] [--visibility public|private] [--readme <path>]
cas project member <who> read|write|maintain|admin|remove
cas project protect <history> on|off
cas project gate <history> [--approvals <n>] [--checks <a,b>] [--resolved]
cas project label <name> [--color <c>] [--set <s>] [--rule one|any]
cas project label <name> remove
cas project webhook list
cas project webhook add <url> [--secret-file <f>] [--events <k,..>]
cas project webhook remove <id>
cas project runner list
cas project runner <account> add|remove
```

Make and manage a project.

### histories

```
cas histories
```

The project's histories and their tips.

### blame

```
cas blame <path>
```

Each line of <path> with the commit that last changed it.

### checks

```
cas checks
```

The check results of a commit.

### proposal

```
cas proposal list [--state open|merged|closed|all]
cas proposal open <title> --source <history> [--target <history>]
cas proposal show <n>
cas proposal edit <n> --title <t>
cas proposal close|reopen|approve|merge <n>
cas proposal thread <n> <path>:<line> [--old] <text>|- [--suggestion <text>]
cas proposal reply <n> <thread> <text>|- [--suggestion <text>]
cas proposal resolve|promote <n> <thread> [--undo]
```

Propose, review and merge a change.

A thread is a review thread on a line of the diff (--old: the line's old side). promote makes a thread's note the merged step's note on its line.

### issue

```
cas issue list [--state open|closed|all] [--label <l>]
cas issue open <title> [--body <text>|-] [--label <l>]...
cas issue open <title> --at <path>:<line> [--suggestion <text>]
cas issue show <n>
cas issue comment <n> <text>|-
cas issue edit <n> [--title <t>] [--body <text>|-] [--assign <a,b>]
cas issue close|reopen <n>
cas issue label <n> +<add> -<remove>...
```

Issues of a project.

### release

```
cas release list
cas release publish <version> --changelog <file> [--change <kind>]
cas release archive <tag> <file.tar.gz>
```

Releases of a project.

### mirror

```
cas mirror add <name> <git url>
cas mirror pull <name>
```

Mirror a git repository.

### events

```
cas events [--after <n>] [--follow]
```

The event log of a project.

--follow waits for new events.

## Lean proofs

### lean

```
cas lean import <export-file>
cas lean import --root <lake-dir> <module>...
cas lean prove <name>... [--all] [--closure] [--budget <rows>]
cas lean check <name>... [--all] [--closure] [--quiet]
cas lean show <name>
cas lean axioms <name>...
cas lean report [--failed] [--slowest <n>]
```

Store Lean declarations, prove them and check the proofs.

import stores each declaration of a Lean export as its own blob, and an environment naming them all; with --root it runs the exporter in a Lake project, with that project's Lean. Importing again after an edit writes only the declarations that changed.

prove writes, for each declaration, the steps of Lean's kernel that accept it, and stores them. A proof that is stored and was made against the declarations the environment has now is kept, so after an edit only what changed and what rests on it is proved again. An inductive type is proved with its whole block, constructors and recursors included.

check trusts nothing stored: it checks every step of a proof, that the proof is of this declaration, and that each constant the proof cites is the environment's declaration of that name. With --closure (or --all) it also checks everything the named declarations rest on. It fails when anything is refused, and says why.

A declaration can be named by its Lean name or by the key of its blob. show prints one declaration, axioms the axioms the named ones rest on, and report sums up the stored results.

- `--root <lake-dir>`: the Lake project whose Lean exports the modules
- `--env <name>`: the Lean environment to work in (default: default)
- `--closure`: also everything the named declarations rest on
- `--budget <rows>`: give up on a declaration after this many rows (default 2000000)
- `--all`: every commit the commit reaches, not only the commit itself
- `--quiet`: print the outcome only, not a line for each proof
- `--failed`: list the declarations that were not proved
- `--slowest <n>`: list the n declarations that took longest to prove

## The tool

### help

```
cas help [<command>]
cas <command> --help
```

Explain cas, or one command.

`help --man` prints the manual page and `help --markdown` the command reference.

### completions

```
cas completions fish|bash|zsh
```

Print the completion script for a shell.

fish: cas completions fish > ~/.config/fish/completions/cas.fish

bash: cas completions bash > ~/.local/share/bash-completion/completions/cas

zsh: cas completions zsh > a file named _cas in a directory of $fpath

### version

```
cas version
cas --version
```

Print the version of cas.
