# Sync (Stage 7)

Two stores converge using only the core ops. There is no negotiation and no have/want exchange:
the side doing the sync walks formats from a reference, asks the destination whether it holds
each key, and writes what is missing. Any store, cache or server can take either role, since
they all speak the same four ops (the tests run one side over HTTP).

## Existence

`has(store, key)` lists the one-key span `[key, key + 0x00)`. That answers whether a key is
present without transferring its value.

## Copying a reference

`copy(src, dst, root)` writes everything reachable from `root` that `dst` lacks:

- **Children come from formats.** A directory's children are its entries. A `record`'s are its
  version and its parents. File formats and unknown formats have none, so their blobs are copied
  without being followed.
- **Children before parents.** A blob is written only after everything it references. This keeps
  the **closure invariant**: if a store holds a blob, it holds everything reachable from it.
- **Stop at what is there.** Because of the invariant, the walk stops descending at the first blob
  the destination already has. Copying a commit on top of a synced history costs the new blobs
  (one per changed directory, plus files and the record), not the size of the repository.
- **Formats travel too.** For each format seen, its format object and spec are copied as well, so
  the destination can describe what it holds.
- **Interruptions are safe.** An interrupted copy leaves only complete subtrees behind, and
  running it again resumes where it stopped.

The invariant holds for stores written by this tooling. A store that receives blobs some other way
must keep it too, or a sync may skip a subtree it believes is present.

## Branches

`push(src, dst, tag)` makes the branch `tag` in `dst` point at its head in `src`:

- If the heads are equal, it is up to date.
- If `dst` has no such tag, it copies the history and creates the tag (expect-absent).
- If `dst`'s head is an ancestor of `src`'s head, it copies what is missing and moves the tag by
  compare-and-swap from the old head (fast-forward).
- Otherwise the branches have **diverged**, and nothing is overwritten. The same happens if `dst`
  moved during the copy. Merging is left to the caller (deferred in the handoff), and the copied
  blobs stay useful for it.

Pulling is a push in the other direction. Derived results can be synced the same way, by copying
the result and creating its `derived/F/X` tag with expect-absent.

## Conformance

`version/tests/sync.rs`, run against an in-memory store and against one over HTTP, checks:

- that a first push copies exactly the reachable blobs plus format descriptions, and a one-file
  commit copies four blobs;
- up to date and fast-forward pushes;
- that diverged branches are refused and left unchanged;
- that interrupted copies leave walkable subtrees and complete when run again;
- that `has` does not match prefixes.
