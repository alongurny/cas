# Function: heads

Input: a history (format `history`). Output: a `uni-bin` map

    {'tags': {NAME: NODE, ...}, 'tips': {NODE: {PRED: {}, ...}, ...}}

`tags` is the history's tags, each naming its node id. `tips` holds every
tip (a node no other node follows), each mapping to the set of node ids it
follows directly. An empty map is left out, as in the history format.

It answers what most commands ask of a history (the latest node, a tag,
the step into the latest node) without reading the whole history, whose
size grows with every node. It is cheap to compute when a history is
written, since the writer holds the history.
