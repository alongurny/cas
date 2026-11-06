# Derived data (Stage 6)

Derived data is a cache of f(X): the result of a deterministic function f over an input X, which is
a version (or one of its directories) or a history. Everything else, such as indexes,
summaries, blame or path logs, is built from such results.

## Functions

A function is identified by the key of its **function object**, a `uni-bin` map:

    {'input': <format id>, 'name': <label>, 'output': <format id>, 'spec': <blob key>}

The `input` format is what X must be in. The `output` format is what results are in (`uni-bin` for
every function so far). `spec` is the key of the document defining the function, and `name` is
only a label. As with formats, changing a definition gives a new id, so results of old and new
definitions never mix. `derived::publish` writes the object and spec into a store. Later, a spec
could point to code, for example a Uniform program, and any store could then recompute a result
to check it.

## Results

The result of function F over input X is an ordinary blob. The tag

    derived/F/X

where F is the function id and X is the input's key, holds the text of a reference to it.
`derive(F, X)`:

1. reads the tag and returns the reference if it is set;
2. otherwise computes the result, writes it, and creates the tag with expect-absent;
3. if the tag was created meanwhile (`conflict`), returns that result instead. Functions are
   deterministic, so both are the same value.

The tag is an ordinary tag; there is no write-once mechanism. Computation can be eager (after a
commit) or lazy (on first use). Results never change the inputs they describe.

A function may derive from other derived results. `summary` sums its subdirectories' summaries, so
after an edit only the directories on the edited paths are recomputed. This is principle 9 (cost
follows the change) applied to derived data.

## Functions so far

- [summary](functions/summary.md): number of directories and files under a directory, computed
  recursively.
`path_log(history, path)` walks the history's log and keeps the nodes where the object at `path`
differs from every predecessor's (a root counts when `path` exists). Each test is one lookup per
side, so it needs no cache: its cost follows the depth of `path`.

## Conformance

`version/tests/derived.rs` checks function ids and objects, cache hits (no writes on a second
derive), incremental recomputation after one edit (at most six writes in a 111-directory tree),
that a result recorded first wins, input-format checks, and `path_log` across a linear history
and a merge.
