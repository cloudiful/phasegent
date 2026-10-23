# Collection pagination

The nine supported collection commands default to page 1 with 50 items in both
text and JSON output: `worktree list`, `worktree status`, `timer list`,
`project list`, `version list`, `comment list`, `record list`, `relation list`,
and `issue branches`.

Use `--page N` for a positive, 1-based page and `--limit N` for a page size
from 1 through 100. Either option can be used alone: `--limit` starts at page 1,
and `--page` uses a limit of 50. Invalid, overflowing, or oversized offsets are
rejected before storage or provider access. There is no automatic full-collection
traversal for ordinary CLI calls.

```sh
phasegent --output json worktree list --page 2 --limit 50
phasegent comment list 123 --limit 20
phasegent record list 123 --kind executor --phase P1 --page 1
phasegent timer list --status running --page 2
```

## Result

Paged results share this core envelope:

```json
{"items":[],"page":1,"limit":50,"total_count":0,"has_more":false}
```

Command-specific scope fields remain alongside it:

| Command | Context fields |
| --- | --- |
| `worktree list` | `repo_identity` |
| `worktree status` | `issue` |
| `timer list`, `project list`, `version list` | none |
| `comment list`, `record list`, `relation list` | `issue` |
| `issue branches` | `issue`, `repo_key`, `local_only`, `ambiguous` |

`total_count` is exact after filtering when known. It is `null` only when a
native upstream omits its total; a bounded lookahead determines `has_more` in
that case. An incomplete upstream page or failed lookahead is an error. An
out-of-range page has empty `items` and `has_more:false`, while retaining the
requested page and collection metadata. Text output shows the current page,
total and whether more items remain, together with the entries and scope.

Worktree status counts active leases only. Record filters and decoding run
before counting and slicing; ordinary comments do not count as records. Branch
ambiguity reflects all matching repository/issue scopes, not only the visible
page. Separate page requests do not share a snapshot, so concurrent collection
changes can shift offsets.

## Read behavior

| Collection | Local | Redmine |
| --- | --- | --- |
| Worktree list/status | Native SQLite count and `LIMIT/OFFSET` | Not provider-backed |
| Timer list | Native SQLite count and `LIMIT/OFFSET` | Not provider-backed |
| Project list | Native database page | One native page; at most one limit-1 lookahead if the total is omitted |
| Version list | Empty catalogue | One native page; at most one limit-1 lookahead if the total is omitted |
| Comment list | Native database page | **Output-only:** complete journals are fetched, then sliced |
| Record list | **Output-only:** complete comments are decoded and filtered, then sliced | **Output-only:** complete journals are decoded and filtered, then sliced |
| Relation list | Not supported | **Output-only:** complete relations are fetched, then sliced |
| Issue branches | Native branch-link page/count; ambiguity across all matching scopes | No provider request |

Output-only pagination limits displayed results, not network transfer or record
decoding. Project and version pages do not fetch the full catalogue. The local
version catalogue is empty.

`issue search` has its own existing `--page`/`--limit` interface and result
contract; it is not part of this collection envelope. `issue get` still takes
explicit IDs, up to 20 at once. Status catalogs, config, and doctor are not
paginated.

## Worktree maintenance

Pagination applies only to the lease query. Authorized Operator/orchestrator
`worktree list` callers still complete the repository-scoped synchronous
reconciliation before reading the requested page; `--no-sync` skips it. Read
roles do not trigger reconciliation. Acquire, sync, prune, and workflow
maintenance use their complete collections, never the CLI page limit. See
[worktree maintenance](worktree-maintenance.md).

The OpenCode adapter explicitly follows active-lease and lease-history pages
when it needs complete collections. It validates every page and returns no
collection if a later read fails or the result is malformed or inconsistent.