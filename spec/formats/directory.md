# Format: directory

A directory: named children, each a file, an executable file, a symbolic
link or another directory. It is given as a type ([types.md](../types.md)):

    directory = sorted<{ child: key<raw | symlink | directory | exec>, name: name }> by name

So a blob is one entry after another, each written as the child's key (32
bytes), its tag (1 for `raw`, a file's contents; 2 for `symlink`; 3 for
`directory`; 4 for `exec`, the contents of a file that may be run), the
length of the name (1 byte), the name, and zero bytes up to the next
multiple of 64. A name of up to 30 bytes makes an entry of one block; a
longer name simply runs on into the following blocks.

Names are strictly ascending, compared bytewise. The empty blob is the
empty directory.

The version before this one had no `exec`. Every blob valid then is valid
now and says the same.
