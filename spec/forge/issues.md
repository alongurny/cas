# Forge: issues

Issues are a history, `p/<owner>/<project>/i`: each change to an issue is
a step. A version of it holds, for issue `n`:

- `<n>/issue.uni`, a `uni-txt` document: `title`, `state` (`open` or
  `closed`), `author`, `created`, `updated` (time), `labels` (set),
  `assignees` (set), `body`, and for review findings `anchor`
  (`{path, line, quote}`) and `suggestion` (the replacement for `quote`);
- `<n>/c/<k>.uni`, comment `k` (a number from 1): `author`, `time`, `text`.

Numbers come from the project's counter ([project](project.md)). Two
people commenting at once write different files, so the later write is
rebased and both stay. Labels in a set with rule `one` are exclusive:
setting one removes the others of its set.
