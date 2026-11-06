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

## Branches: `merge_branch(tag, theirs, meta)`

- If theirs is already in the branch's history, the branch is up to date.
- If the branch head is in theirs's history, the branch fast-forwards to theirs.
- Otherwise the base is the best common ancestor. After a criss-cross there are several; their
  versions are merged recursively into a virtual base, with conflicts inside it resolved to the
  first ancestor's side. Unrelated histories merge against the empty tree.
- A clean merge writes a record with both heads as parents and moves the tag by compare-and-swap.
  On conflicts nothing is recorded. The caller gets the merged version (ours at each conflict) and
  the conflict list, resolves them, and commits a record with both parents.

After `sync::push` reports a diverged branch, pull the other head under another tag, merge, and
push again.

## Conformance

`version/tests/merge.rs` covers:

- diff3 cases;
- path-level merges: disjoint edits, identical edits, edit/edit, remove-directory/edit-inside,
  replace-directory/add-inside, and new directories on both sides;
- a line-level merge inside one file;
- fast-forward, up-to-date, merge-record and conflict outcomes on branches;
- a criss-cross merge through a virtual base;
- property tests on random trees: merging with an unchanged side gives the other side, identical
  sides merge to themselves, and clean merges are symmetric.
