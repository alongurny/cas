# Format: annotation

Notes laid out as a tree that mirrors a directory tree: why lines changed
in a step, or what the lines of a version are for. It is given as a type
([types.md](../types.md)):

    annotation = { note: key<note>?,
                   entries: sorted<{ child: key<annotation | lines>, name: name }> by name }

So a blob is one block holding the key of a note on the directory as a
whole (zeros for none) and zeros; and then one entry for each name under
the directory that has notes, written as a `directory` entry is: the
child's key, its tag (1 for an `annotation` of a directory below, 2 for
the `lines` of a file), the name's length, the name, zeros to the next
multiple of 64. Names are strictly ascending.

A path is not written anywhere: it is the names on the way from the top
annotation down to a blob.
