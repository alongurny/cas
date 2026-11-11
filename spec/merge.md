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
its base, ours and theirs entries. A clean merge is symmetric: swapping ours and theirs gives the
same version.

## Histories

A merge and a rebase work on commits ([formats/commit.md](formats/commit.md)).

- `merge(ours, theirs)`: up to date when `ours` reaches `theirs`; a fast
  forward when `theirs` reaches `ours`. Otherwise the base is the newest
  commit both reach, and a clean merge makes a commit of the merged version
  with both as parents and no step notes. On conflicts nothing is written;
  the caller gets the merged version (ours at each conflict) and the
  conflict list. Two histories that share no commit are not merged.
- `rebase(ours, onto)`: the commits `ours` has and `onto` lacks (they must
  form one line, with no merge among them) are replayed onto `onto`, oldest
  first: each is merged three-way with its parent as base, the replayed tip
  as ours and the commit as theirs. Authors and times stay. A step keeps
  its annotation where its line runs still fit the files, else its notes
  without the runs. A conflict stops the rebase and returns it.

## Conformance

`version/tests/merge.rs` covers:

- diff3 cases;
- path-level merges: disjoint edits, identical edits, edit/edit, remove-directory/edit-inside,
  replace-directory/add-inside, and new directories on both sides;
- a line-level merge inside one file;
- property tests on random trees: merging with an unchanged side gives the other side, identical
  sides merge to themselves, and clean merges are symmetric.
