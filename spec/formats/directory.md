# Format: directory

A directory: named children, each a file, a symbolic link or another
directory. It is given as a type ([types.md](../types.md)):

    directory = sorted<{ child: key<raw | symlink | directory>, name: name }> by name

So a blob is one entry after another, each written as the child's key (32
bytes), its tag (1 for `raw`, a file's contents; 2 for `symlink`; 3 for
`directory`), the length of the name (1 byte), the name, and zero bytes up
to the next multiple of 64. A name of up to 30 bytes makes an entry of one
block; a longer name simply runs on into the following blocks.

Names are strictly ascending, compared bytewise. The empty blob is the
empty directory. There is no executable kind.
