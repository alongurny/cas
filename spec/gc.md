# Reachability, collection and purging

The core never deletes on its own: a blob stays until someone removes it.
This layer decides what may go: only blobs nothing reaches. Code:
`version/src/gc.rs`.

## Reachability

Roots are variables. A variable whose value parses as a view `<format> <key>`
roots that blob; other variable values root nothing. From each rooted view, format walkers follow children the same
way sync does: a `dir` follows its entries, a `history` its nodes'
versions. Formats this tooling does not know are opaque and have no children.

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
