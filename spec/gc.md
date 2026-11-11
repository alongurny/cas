# Reachability, collection and purging

The core never deletes on its own: a blob stays until someone removes it.
This layer decides what may go: only blobs nothing reaches. Code:
`version/src/gc.rs`.

## Reachability

Roots are variables. A variable whose value parses as a view `<format> <key>`
roots that blob; other variable values root nothing. From each rooted view, format walkers follow children the same
way sync does: a directory follows its entries, a commit its version, its author, its
notes and its parents. Formats this tooling does not know are opaque and have no children.

Also reachable:

- the format object of every view met, and the `spec <key>` it names;
- for a variable `derived/F/X`, the function object `F` and its `spec`, so a
  cache entry keeps its own description alive;
- the metaformat document.

A missing blob ends the walk at that point without an error.

Derived variables are roots like any other, so a cache keeps its results alive.
Dropping a cache means removing its `derived/` variables.

## Collection: two phases

A writer puts children before parents and links the top last (a variable write).
Between the blob writes and the variable write, the new blobs are unreachable. To
avoid racing writers, `collect` never removes a blob the first time it sees
it unreachable:

1. Read variable `gc/pending`, a `uni-bin` view of a set (map to `{}`) of
   blob keys the previous run found unreachable.
2. Mark from the roots, and list all blobs.
3. Remove every blob unreachable now and listed in the pending set.
4. Write the remaining unreachable blobs as the new pending set and move
   `gc/pending` to it, with compare-and-set on the old value. Two collectors
   running at once conflict here; the loser just stops.

So a writer has from one run to the next to link what it wrote. The
pending sets are themselves blobs: the current one is rooted by its variable;
older ones become garbage and follow the same two runs.

### Runs since the last full one

A full run (`cas gc --full`, or whenever the next paragraph does not apply)
writes the set it marked as a blob (`CASMARK1`, a count, the sorted blake3
digests, then any other keys one per line) and points variable `gc/marked`
at it. A blob in that set was reachable with everything under it: a blob's
children are written before it, and a run that met a missing blob records
no set (and removes `gc/marked`).

A later run reads the set and does not walk into it. Whatever became
reachable since is a new blob, reached from a root through new blobs only,
so walking the roots without entering the set finds all of it. A history of commits
is walked back only to the commits the set holds. Unreachable is then what is in neither set; the two phases work
as above. A blob of the set that became garbage waits for the next full
run, which comes when the blobs outside the set are more than a quarter of
it.

On a local store, `gc` then repacks (spec/performance.md). The packs a
repack writes are sealed: their deltas are on bases in sealed packs. When
at most an eighth of the sealed packs' entries were removed, the repack is
incremental: the sealed packs stay, and everything else (loose files and
the packs commits and pulls wrote) goes into a new sealed pack, with delta
hints only for the commits made since the last repack. The sealed
packs are merged as a binary counter, so there are O(log n) of them. Past
an eighth, or with `--full`, everything is rewritten into one pack and the
removed names' bytes go. Chunks no blob names are swept on a full run, or
when loose chunk files exist.

On git.git, a gc after one commit takes 0.8 s this way (14 s rewriting
everything; git's gc 13.5 s).

### Limits

- A blob referenced only from inside an opaque format is not protected.
  Root it with a variable, or teach the tooling the format.
- A writer that dedups (skips writing a blob because it is present) might
  rely on a blob a collector is about to remove. Writers that run longer
  than one collection interval must re-check with `has` before linking, or
  write the blob again (writing an existing blob is harmless).

## Deleting content on purpose

Not supported. Removing a blob that versions still use does not make
its content secret: the key stays in every tree that lists it, and a key
reveals content drawn from a small space (a short password, say) to a
search. Secure deletion needs its own design and is out of scope for now.
