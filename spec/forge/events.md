# Forge record: the event log

Everything that happens in the forge is an event, appended to a log kept
in segments. Variable `forge/events` holds the newest segment, a
[record](store.md) whose `record.uni` has:

| Field | Value |
| --- | --- |
| `first` | the number of the segment's first event |
| `events` | list of events, by number |

and, unless it is the first segment, `prev`: a view of the previous
segment. A segment holds at most 256 events; the next event starts a new
segment pointing at the full one.

An event's fields: `time`, `actor` (account name, or empty), `kind`, and,
by kind, `project` (`<owner>/<name>`), `var` (variable name in the
project), `old` and `new` (views, or empty), `number` (an issue or
proposal), `text`.

Kinds: `move` (a variable moved, on the wire or through the forge),
`project`, `member`, `proposal`, `review`, `merge`, `issue`, `comment`,
`release`, `check`, `account`.

The forge serves the log as server-sent events at `/api/v1/events`
(`?after=<number>` resumes) and sends it to webhooks: a POST of the event
as JSON, with header `X-Cas-Signature: sha256=<hex>`, the HMAC-SHA256 of
the body under the webhook's secret. Deliveries are tried three times
and are not recorded.
