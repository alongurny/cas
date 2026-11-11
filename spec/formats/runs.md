# Format: runs

Notes on the lines of a file, as runs of lines that share a note. It is
given as a type ([types.md](../types.md)):

    runs = runs<{ note: key<note>?, count: integer }> by note

So a blob is one block for each run: the key of the note (zeros for lines
without a note), the number of lines the run covers in 8 bytes, at least
1, and zeros. No run has the same note as the run before it, and there is
at least one run.

The counts add up to the number of lines of the file the runs are about.
That is a relation between this blob and the file, checked by whoever
holds both.
