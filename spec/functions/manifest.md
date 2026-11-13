# Function: manifest

Input: a `commit`. Output: a `manifest`.

The output lists every blob the commit brings: the commit itself, and each
blob it reaches through its version, its author and its notes (its own and
those of its steps) that is not in the same place under one of its parents.
Files, and anything reached only through a parent, are left out.

"In the same place" is decided going down from the commit and its parents
together. A blob that one of the parents has in the place being looked at
is left out, with everything under it. Otherwise the blob is listed, and
each key it holds is looked at against the keys in the same field of the
parents' blobs: for a record with a name, the records with that name; for a
head, the heads; for other records, all the records.

The blobs listed so are **new**. The output also has what a proof needs to
find, among the parents, the keys the new blobs share with them: each
parent commit, as a **parent**, and each blob of a parent that was looked
at as the counterpart of a listed blob, as **opened**. A blob that is both
listed and opened is opened only: it is reached from a parent.

So every blob a commit reaches that is not a file is listed by the
manifest of that commit or of a commit before it. A blob may be listed by
more than one (one that comes back at another path, say); that costs a
second proof of it and nothing else.

A commit that brings nothing but itself still lists itself, so the output
is never empty.
