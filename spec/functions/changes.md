# Function: changes

Input: a history record (format `record`). Output: a `uni-bin` set (a map whose values are `{}`)
of path strings: the paths whose node in the record's version differs from that path's node in
every parent's version. Absent counts as a node, so additions and removals are included, and so is
each path under a directory that was added or removed. A record with no parents changes every
path in its version.

A path changed relative to only some parents of a merge came from the other parents unchanged, so
it is not listed.
