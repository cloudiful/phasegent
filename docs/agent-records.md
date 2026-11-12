# Agent records

`record create`/`get`/`list` is a thin wrapper over the tracking provider's
existing issue-comment primitive. It is the documented path for agent notes; the
legacy `comment` commands stay available for historical records and the existing
audit protocol.

## One CLI-owned header

The CLI generates and parses exactly one versioned metadata header on the note
body. An agent supplies metadata (`--key`, `--phase`, `--attempt`, `--review`,
`--recon`) plus a plain note body and never writes a header by hand. The record
kind is derived from the verified host role; there is no `--kind` create flag
and no actor override. The body is opaque to the parser: it is never scanned
for substring authority, and a body that begins with the reserved header prefix
is rejected rather than decoded.

`record get` and `record list` return the plain body plus the structured fields;
the generated header is never shown.

## Tolerant reads, strict writes

A comment that opens with the reserved header but is not a valid record is
never read as a record and never fails the whole read. It is listed and
readable as an **invalid entry**:

- `record list` shows every invalid entry with `status=invalid`, its id,
  its link, the issue, the stored body as `raw`, and the decode failure as
  `error` (`kind=decode`). It carries no kind, actor, key, phase, attempt,
  review, recon, or body: invalid content is never audit metadata.
- A valid list item keeps every record field and adds `status=record`.
- `record get` on an invalid comment returns the same explicit invalid
  object and succeeds; an ordinary comment stays a not-found result.
- Invalid entries are listed whatever `--kind`, `--phase`, or `--recon`
  ask for, because their metadata is not filterable; valid entries obey the
  filters. `invalid_count` counts the invalid entries of the whole
  filtered collection, and a short warning on stderr names them.

A write stays strict. The pre-write scan of the existing comments tolerates
an invalid entry only when its own request key is a different, established
one. An invalid entry that claims the requested key, or whose key cannot be
established at all, refuses the write before anything is written, and the
confirmation read repeats the same check.

The key of an invalid entry is recovered conservatively: exactly one
top-level `key` string that is a valid request token is trusted. A
duplicate key, a non-string value, a missing key, an unterminated header,
or a body that is not a JSON object leaves the key unknown, and an unknown
key is never guessed from prose. No other header field is recovered, and
no invalid header is ever repaired or inferred.

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

The kind is derived from the verified host role; there is no create flag and no
actor override.

- `executor` publishes an `executor` record.
- `reviewer` publishes a `reviewer` record.
- `explore` publishes a `recon` record, its only write, and only with
  `--authorized`.
- The orchestrator owns no record kind and is denied `record create`; it
  publishes comments instead. `admin` is never an agent role and publishes no
  records.

`executor` and `reviewer` notes keep their `status` / `VERDICT:`/`REVIEW:` note
semantics; only the transport (record instead of comment) and the header
ownership (CLI instead of handwritten) change. `record list --kind` remains a
read filter over the stored kinds.

A recon record is evidence for the parent, not an audit note or a VERDICT.

## Redmine and local boundaries

- **Redmine** writes a normal issue note; the standard note write
  (`PUT .../issues/:id.json`) returns `204` with no body, so the CLI recovers the
  created journal by reading the issue
  (`GET .../issues/:id.json?include=journals`) and locating the exact owned
  record. There is no journal-metadata write endpoint, and no server-side journal
  filtering is promised: `record list` filters locally over the decoded journals
  and ignores ordinary comments. A comment whose reserved header is not a valid
  record is listed as an invalid entry instead of failing the read.
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
