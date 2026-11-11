# Format: lines

The notes on one file. It is given as a type ([types.md](../types.md)):

    lines = { note: key<note>?, old: key<runs>?, new: key<runs>? }

So a blob is two blocks: the key of a note on the file as a whole, the key
of the runs over the lines of the file before the step, and the key of the
runs over its lines after the step (or, for the notes of a version, over
its lines as they are), each zeros for none, and then zeros.
