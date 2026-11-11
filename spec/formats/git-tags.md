# Format: git-tags

Git's annotated tags of a project: a `uni-bin` map from tag names (strings) to the ids of their
tag objects (strings of 40 lowercase hex digits). A tag in cas is a name in a history; the tag
object, with its tagger, date and message, is git's, and is kept so a git client gets the tag
it would get from git.

It names each tag object, as a `git-tag` at key `sha1:<id>`.
