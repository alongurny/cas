# Release histories

Two kinds of history serve two audiences ([history.md](history.md)).

- **Internal histories** are where developers work: many small nodes, each
  step with its notes, an [annotated](formats/annotated.md) view of the
  step's [diff](formats/diff.md) that says line by line why each change was
  made, and nodes with annotated files (`'notes'` of a node) for what is
  worth saying about a file as it stands. They replace commit messages.
  They stay with the developers: they are pushed only to stores the team
  shares.
- **A release history** is what clients get: one node for each release,
  after the release before it, its step carrying release notes (the
  `'changelog'`) and whether the upgrade is compatible. It holds no notes of
  any kind, on steps or nodes, and names no internal node: a client learns
  the released versions and what each release says, nothing of how the
  work went.

Both are `history` blobs; a release history is one that holds no notes.
`cas publish PUBLIC -m NOTES` makes the next release: one node with the
internal tip's version after the release history's tip (which the internal
history must follow; rebase first otherwise), with the release notes as its
changelog (the first release, which has no step, keeps them as its node's
`'doc'`). It refuses a target history that holds notes, so annotations
never reach a release history through it, and the version it adds carries
no history of its own: the internal nodes, their diffs and their notes stay
behind. `cas log -H PUBLIC` is then the release log.

Anything can still be pushed anywhere: keeping internal histories off the
clients' store is a matter of which histories are pushed there, and
`publish` writes only the release one.
