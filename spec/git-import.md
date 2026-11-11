# Importing git

`cas import-git <repo> [<rev>] [-H <name>]` turns the commits reachable
from `<rev>` (default `HEAD`) into a new history of commits named `<name>`
(default `main`). It runs `git` and only reads the repository: git packs
the objects `<rev>` reaches, the store takes them in git's own formats
([git-commit](formats/git-commit.md)), and each git commit becomes a commit
the way a forge makes one of a commit pushed to it ([forge/git](forge/git.md)).

| git | cas |
| --- | --- |
| tree | `directory` (entries sorted by name; each git tree converted once) |
| file, mode 100644 | `raw` |
| file, mode 100755 | `exec` |
| symlink, mode 120000 | `symlink` |
| submodule, mode 160000 | left out |
| a name the name rule refuses | held escaped ([types](types.md), "Names from outside") |
| commit | a commit of its tree, after the commits of its parents |
| author | the commit's author, `Name <email>` |
| committer's time | the commit's time |
| commit message | the note of the step from the first parent, its lines that say something joined by ` / `; a root commit's is the note of its version |
| tag | not imported: a history of commits has no tags, and a release is published |

The same git commits always give the same commits. Empty directories do not
exist in git, so none are created. The git objects are not kept by the
history: the next `cas gc` collects them, unless something else in the store
names them.
