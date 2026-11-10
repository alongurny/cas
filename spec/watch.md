# The status watcher

`cas watch <dir>` follows a working directory, as git's fsmonitor does, so
that `commit` and `status` look only at what changed instead of at every
file. It is optional: without a running watcher, both look at everything,
using the stat cache (`.cas-index`) to avoid rehashing unchanged files.

## Files

Both live at the top of the directory and are never imported, like
`.cas-index` and `.cas-sparse`:

- `.cas-watch.lock`: the running watcher holds an exclusive `flock` on it.
  A reader that can take a shared lock knows no watcher runs.
- `.cas-watch`: the log. The first line is `CASWATCH1 <run>`, naming this
  run of the watcher (process id and start time). It is written only after
  every directory is watched, so a log without it is not ready yet. Every
  later line is the path of something that changed, relative to the
  directory: a file written, created, removed, renamed from or to, or
  whose metadata changed. A line `!` means events may have been lost (the
  kernel's queue overflowed, or a name the log cannot hold).

The watcher uses inotify on every directory, and adds a watch for each
directory that appears (logging everything already inside it, which may
have been created before the watch).

## Tokens

A token is (run, length of the log). A commit or status takes one before
it looks at the directory, and the stat cache stores it with the view the
directory was imported as. Next time, if a watcher with the same run is
still running, the paths logged between the stored token and a new one are
everything that may have changed: a change made while the last import was
looking at the directory was logged after its token. So the import edits
only those paths into the stored view:

- a logged path, and everything under it, is looked at again (stat, and a
  hash when the stat cache cannot vouch for it);
- whatever the cache had at or under it and is gone is removed;
- everything else keeps its cached entry and is not looked at.

The stored view must still be in the store; if it is, everything under it
is (the closure invariant). Otherwise, or with no watcher, another run, a
`!` in the range, or a cache without a token, the import looks at the whole
directory, as without a watcher, and stores a fresh token.

## Numbers

git.git checkout, a commit with nothing changed: 50 ms looking at every
file, 15 to 19 ms with the watcher (git's commit of the same: 220 ms).
