# Forge: git over HTTPS

The forge is a git server too. Each project is the git repository
`https://<host>/<owner>/<project>.git` (the `.git` may be left out), which
the git client clones, fetches and pushes as it would any other. Under it
are the project's cas histories; the git objects are stored in the same
store, in git's own formats ([git-blob, git-tree, git-commit,
git-tag](../formats/git-commit.md)), so a commit keeps its id.

## Branches are histories

| Git ref | History |
| --- | --- |
| `HEAD` | `refs/heads/dev` |
| `refs/heads/<x>` (no `/` in `x`) | `h/<x>`: `dev`, `release`, other shared histories |
| `refs/heads/u/<user>/<name>` | `u/<user>/<name>` |
| `refs/tags/<t>` | tag `t` of a history; a tag of `h/release` names the commit of a non-release history with the same version when there is one |

A branch names its history's tip (the newest node when the history forks).

## Links

Variable `p/<owner>/<project>/git/<history>` holds history `<history>`'s
links ([git-links](../formats/git-links.md)): which commit each of its
nodes is. Node ids are a history's own (two commits with one tree are
`v` and `v#1` in the order the history met them), so each history has its
links. A node with no commit yet takes the commit another history of the
project has for the same node id with the same predecessors, themselves
the same commits; so a proposal merged by fast forward puts on `h/dev` the
commit that was pushed. Otherwise it gets a new commit when git first asks
for it: its version's tree, its predecessors' commits as parents, its
step's note (or changelog, or the node's doc) as the message, by
`cas <cas@localhost>` at time 0, so the same history always gives the same
commits. A history that came from git (a push, a mirror, a migration) keeps
git's commits. The links reach the commits, so gc keeps them and blob reads
are scoped by them as by any variable. The variables are readable on the
wire and change only through the forge; deleting a branch removes both.

## Fetch

Protocol version 2 (`ls-refs`, `fetch`), which git speaks by default
since 2.26; older clients are told to set `protocol.version 2`. A caller
may ask only for what the refs name and the commits the project's links
hold. Shallow and partial clones are not served. Packs are sent whole,
without deltas.

## Push

`receive-pack`, with `report-status`, `delete-refs` and `atomic`. Request
bodies may be gzipped and chunked. A push is checked by the wire's rules
([store](store.md)): `h/<x>` needs maintain and must not be protected;
`u/<user>/...` is the user's own and needs write. A tag push is refused
(releases make tags: `cas release publish`). An update must be a fast
forward: the pushed commit's new ancestors become nodes after the
history's tip, each commit's tree a version (the `tree` directory format,
as the importer writes it), its message the note of the step from its
first parent (lines joined with ` / `), a root commit's message its node's
doc. A commit that is already a node of another history of the project
brings that node, and what precedes it, along, with their links. Deleting a branch removes
its history. Each move is an ordinary move of the history variable, by the
pusher, so proposals, checks and events follow as they do on the wire.

## Sign in

HTTP Basic: the user's name and a token, or their password. A request
without credentials to a project the caller cannot read is asked to sign
in (401), which is when git sends them.
