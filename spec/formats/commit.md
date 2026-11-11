# Format: commit

One point in a history: a version, who made it and when, the notes on it,
and for each parent the notes on the step from that parent. It is given as
a type ([types.md](../types.md)):

    commit<T, A> = { version: key<T>, author: key<author>, notes: key<A>?, time: time,
                     parents: sorted<{ commit: key<commit<T, A>>, step: key<A>? }> by commit }

and this format is `commit<directory, annotation>`.

So a blob is two blocks, holding the keys of the version, the author and
the notes (zeros for none), then the time in 8 bytes and zeros; and then
one block for each parent, holding the parent commit's key and the key of
the step's annotation (zeros for none). Parents are strictly ascending by
key. A commit with no parents starts a history.

A commit's key is its id. It fixes the commit's whole past, since each
parent is named by key. A history is not a blob: it is a variable that
holds the keys of its newest commits.
