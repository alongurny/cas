# Format: release

A released version: a tag and a key, and how it relates to the releases
before it. It is given as a type ([types.md](../types.md)):

    release<T> = { version: key<T>, tag: key<tag>,
                   from: sorted<{ release: key<release<T>>, changelog: key<note>,
                                  change: breaking | feature | patch | security }> by release }

and this format is `release<directory>`.

So a blob is one block, holding the keys of the version and of its tag;
and then two blocks for each earlier release it follows from: that
release's key, the key of the changelog (prose: what is different), and
one byte for the kind of change: 1 `breaking` (users may have to change
something), 2 `feature` (adds, breaks nothing), 3 `patch` (fixes, adds
nothing), 4 `security` (a patch that should not be skipped). Earlier
releases are strictly ascending by key.

The releases of something form a graph. Upgrading from one release to
another is safe when some path between them has no `breaking` step. A
release carries no notes.
