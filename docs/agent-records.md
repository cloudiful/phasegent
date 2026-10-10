# Agent records

`record create`/`get`/`list` is a thin wrapper over the tracking provider's
existing issue-comment primitive. It is the documented path for agent notes; the
legacy `comment` commands stay available for historical records and the existing
audit protocol.

## One CLI-owned header

The CLI generates and parses exactly one versioned metadata header on the note
body. An agent supplies metadata (`--kind`, `--key`, `--phase`, `--attempt`,
`--review`, `--recon`) plus a plain note body and never writes a header by hand.
The body is opaque to the parser: it is never scanned for substring authority, and
a body that begins with the reserved header prefix is rejected rather than decoded.

`record get` and `record list` return the plain body plus the structured fields;
the generated header is never shown.

## Stable request key

`--key` is a stable request token: 1..128 characters from `[A-Za-z0-9._:-]`. Reuse
it only to retry one identical logical request:

- an identical retry (same metadata and body, same actor and kind, same issue)
  returns the existing record id;
- a changed body or metadata under an already-used key is a **conflict** and
  errors;
- more than one exact same-key record on the issue errors instead of guessing.

This is not atomic deduplication: concurrent ambiguous requests are reported as an
error, never merged. A new attempt or round uses a new key.

## Reference protocol

The record id is the native reference, so evidence is reused instead of
transcribed:

- Redmine — `#change-<journal id>` (the global journal id, not the ordinal
  `#note-<n>` anchor).
- Local — the existing `#note-<id>` anchor, unchanged.
- A parent cites the record id (`record get <issue> <record_id>`) in the issue
  plan rather than copying raw recon. Conclusions in a record are **evidence**:
  they never grant scope, architecture, or authorization the parent did not
  already have.

## Kinds and roles

The kind is bound to the CLI session role; there is no actor override.

- `executor` and `reviewer` notes keep their `status` / `VERDICT:`/`REVIEW:` note
  semantics; only the transport (record instead of comment) and the header
  ownership (CLI instead of handwritten) change.
- `recon` is the explorer's only write, and only with `--authorized`.
- An orchestrator may publish any kind; `admin` publishes no records.

A recon record is evidence for the parent, not an audit note or a VERDICT.

## Redmine and local boundaries

- **Redmine** writes a normal issue note; the standard note write
  (`PUT .../issues/:id.json`) returns `204` with no body, so the CLI recovers the
  created journal by reading the issue
  (`GET .../issues/:id.json?include=journals`) and locating the exact owned
  record. There is no journal-metadata write endpoint, and no server-side journal
  filtering is promised: `record list` filters locally over the decoded journals
  and ignores ordinary comments (a malformed reserved header errors).
- **Local** uses the same comment primitive with the existing `#note-<id>` id and
  the derived `role`/`phase`/`attempt` columns.
- No new tables or migrations are involved: records are rows in the existing
  comment storage, and the provider tables and their historical rows are
  unchanged.

## HTTP limitations

- A note write on Redmine is `PUT .../issues/:id.json` and returns `204 No
  Content`; the created journal id only arrives through the include-journals read,
  which is why an uncertain write triggers a bounded recovery listing instead of a
  blind re-write.
- Records never require a live journal mutation to be verified: the provider
  contract tests and their mock server cover the wire behavior.
