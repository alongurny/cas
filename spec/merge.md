# Merge

Merging combines two versions that both descend from a base. It works on the two diffs from the
base (base → ours and base → theirs), so its cost follows the changes, not the size of the tree.

## Versions: `merge_versions(base, ours, theirs)`

For every path P that theirs changed:

- If ours changed P too, the merge is clean only when both made the same change.
- Otherwise P **conflicts** if ours turned a directory above P into something else (removed it or
  replaced it with a file), or if theirs removed or replaced P while ours changed something under
  it differently.
- Otherwise their change applies.

When both sides changed the same text file (`raw` or `exec`, both valid UTF-8, same format), a
line-level three-way merge (diff3) is tried first. Chunks between lines that both sides kept come
from whichever side changed them; a chunk both changed differently makes the path a conflict.

The result is ours with their non-conflicting changes applied. At each conflict it keeps ours,
and nothing of theirs under or above a conflicted path is applied. Each conflict is reported with
its base, ours and theirs nodes. A clean merge is symmetric: swapping ours and theirs gives the
same version.

## Histories

There are no parents, so a single version has nothing to merge. Merging is
a history operation (spec/history.md), and rebasing is preferred to it.

- `merge_nodes(history, a, b, step)`: if `a` already follows `b`, up to
  date. Otherwise the base is the best common ancestor in the history.
  After a criss-cross there are several; their versions are merged
  recursively into a virtual base, with conflicts inside it resolved to
  the first ancestor's side. Unrelated lines merge against the empty
  tree. A clean merge adds a node following both, with `step` from each.
  On conflicts nothing is added; the caller gets the merged version (ours
  at each conflict) and the conflict list.
- `combine(ours, theirs)`: the union of two histories (ours wins on
  steps, tags and docs), so two diverged copies of a history can be merged
  as two tips of one.
- `rebase(mine, onto)`: the nodes `mine` has beyond the newest node it
  shares with `onto` (they must form one line) are replayed onto `onto`'s
  tip, oldest first: each is merged three-way with its predecessor as
  base, the replayed tip as ours and the node as theirs. Steps and docs
  are kept. The result is `onto` plus the replayed line, so it extends
  `onto`. A conflict stops the rebase and returns it.
- `publish(mine, public, step)`: one node with the version of `mine`'s
  tip, after `public`'s tip, with `step`. `mine` must already contain
  `public`'s tip (rebase first). The private nodes are not copied.

## Conformance

`version/tests/merge.rs` covers:

- diff3 cases;
- path-level merges: disjoint edits, identical edits, edit/edit, remove-directory/edit-inside,
  replace-directory/add-inside, and new directories on both sides;
- a line-level merge inside one file;
- merging two tips of a history: clean, up to date, and conflicts that add nothing;
- rebasing private work onto a moved public history, publishing it as one node, and a rebase
  that stops at a conflict;
- a criss-cross merge through a virtual base;
- property tests on random trees: merging with an unchanged side gives the other side, identical
  sides merge to themselves, and clean merges are symmetric.
