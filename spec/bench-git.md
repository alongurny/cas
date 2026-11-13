# cas against git

`bench/git-vs-cas.sh REPO` runs the same operations with git and with the
`cas` command on one repository. Measured on 2026-11-13 at 3601a14, on
histories of commits, on the Thesis repository (4,579 first-parent commits,
1,292 files at the tip), release build, 4 cloud Xeon cores at 2.3 GHz,
servers on localhost. git's transport is its pack protocol over a local
pipe (`file://`); cas's is HTTPS, its only transport. Times are seconds of
one run each.

| operation | git | cas |
| --- | --- | --- |
| import from git (cas only) | | 59.8 |
| store size, MB | 105 | 133 |
| log, every commit | 0.045 | 0.092 |
| list paths of the tip | 0.003 | 0.005 |
| history of one file | 0.060 | 0.264 |
| check out the tip | 0.446 | 0.291 |
| commit one edit | 0.153 | 0.050 |
| store growth from that commit, KB | 14 | 13 |
| clone everything over the network | 2.72 | 4.00 |
| fetch one file of the tip over the network | 0.033 | 0.005 |
| push one new commit | 0.025 | 0.050 |

Fetching one file: git needs a partial, shallow clone (`--filter=blob:none
--depth 1`, which the server must allow) and then `git show`; cas reads the
commit, the directories on the path and the file, one request each.

The day-to-day path, every Thesis commit replayed through `cas commit` in
order (149 s for 4,579 commits): the store is 460 MB until `cas gc --full`
repacks it with deltas against each path's previous version (29,738
deltas), and 90 MB after, against git's 105 MB.

## git.git

git.git cannot be imported on histories of commits: it has paths the name
rule refuses (`git-lost+found.txt`, from 2005), and the tool does not
rename (spec/types.md). Its numbers on the earlier history format are in
[perf-port.md](perf-port.md)'s history.

## Where cas loses

1. **History of one file**, 4 times git: path-log diffs each commit's
   directories down the path; git compares tree ids in one process.
2. **log**, 2 times git, at 90 ms for 4,579 commits: each commit blob is
   read and its hash checked (kept on every read, Alon's choice).
3. **Clone**, 1.5 times git: cas copies blob by blob in batches, over TLS;
   git sends one pack over a pipe.
4. **Store right after import**, 133 MB against 105. A `gc --full` repacks
   it to 88 MB (two runs: the git objects go in the second). Before
   this round, the second run grew it to 347 MB: frames whose delta base it
   removed were stored whole. Now they try their hinted base, and a full
   run hints every version again.

The measurements on the earlier history format are in this file's history
(before 3601a14).
