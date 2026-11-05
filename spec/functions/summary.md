# Function: summary

Input: a directory (format `dir`). Output: a `uni-bin` map

    {'dirs': D, 'files': F}

where D and F are decimal strings with no leading zeros. They count the directories and the files
(non-directory nodes) at any depth under the input, the input itself excluded.

A subdirectory's counts are the summary of that subdirectory, so an implementation may derive
them recursively and reuse cached results.
