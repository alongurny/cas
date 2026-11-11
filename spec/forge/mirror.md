# Forge record: mirror

A project can follow a git repository. Variable
`p/<owner>/<project>/m/<name>`, a [record](store.md) whose `record.uni`
has `url`, `last` (time of the last pull), `commits` (count), `tip`
(the git commit of the default branch last imported) and `packed` (the
commits, space-separated, whose objects the store already holds). The forge fetches
into a bare repository it keeps only as a cache.

Git branches map onto histories: the default branch to `h/dev`; any
other branch `b` to `u/<owner>/b` (a `/` in `b` becomes `-`). A tag whose
name is a semantic version, with or without a leading `v`, becomes a
release of `h/release` (a later one a `breaking` change, its changelog the
tag's message); a tag with any other name is not a release, and only an
annotated one is kept, as git's tag object. A pull converts the commits
the links do not have yet; since a git commit always gives the same
commit, the new history extends the old one. A default branch the
remote rewrote is refused; any other branch follows the
remote even then, since a force-pushed working branch is the remote's state.  Git's own objects are stored
too (those new since `packed`), and each history's links
([git](git.md)) say which git commit each imported commit is, so the project
served as git has the remote's commit ids.

Export goes the other way, `cas-forge export`: a history becomes a git
branch with the commits git is served ([git](git.md)), and each release a
tag. Exporting `h/dev` keeps a git mirror, such as GitHub's, current.
