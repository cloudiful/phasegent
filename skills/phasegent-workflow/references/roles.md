# Role capability matrix

Source of truth: `src/policy.rs` (`Role::allows`). The five roles are
`admin`, `orchestrator`, `executor`, `reviewer`, `tester`. `--role` is a
capability/routing policy, not identity isolation; each role's credential stays
least-privilege and never crosses roles.

Legend: `✓` allowed, `—` denied.

| Capability | Operation | admin | orchestrator | executor | reviewer | tester |
|---|---|---|---|---|---|---|
| IssueRead | issue read | — | ✓ | ✓ | ✓ | ✓ |
| IssueSearch | issue search | — | ✓ | — | — | — |
| IssueCreate | issue create | — | ✓ | — | — | — |
| IssueUpdateBody | issue update | — | ✓ | — | — | — |
| IssueClose | issue close | — | ✓ | — | — | — |
| IssueAttachmentUpload | issue upload-attachment | — | ✓ | — | — | ✓ |
| RepoCreate | repo create | — | ✓ | — | — | — |
| CommentCreate | comment create | — | ✓ | ✓ | ✓ | ✓ |
| CommentRead | comment get | — | ✓ | ✓ | ✓ | ✓ |
| CommentFindMarker | comment find-marker | — | ✓ | ✓ | ✓ | ✓ |
| ProjectRead | project list | ✓ | ✓ | ✓ | ✓ | — |
| ProjectCreate | project create | ✓ | ✓ | — | — | — |
| IssueStatusRead | issue status list | ✓ | ✓ | ✓ | ✓ | — |
| VersionRead | version list | ✓ | ✓ | ✓ | ✓ | — |
| RelationRead | relation list | — | ✓ | ✓ | ✓ | — |
| RelationCreate | relation create | — | ✓ | — | — | — |
| RelationDelete | relation delete | — | ✓ | — | — | — |

## Notes

- **orchestrator** allows every capability, and is the only role with issue
  write/search/close, repo create, relation write, and the only non-admin
  status `set`/`advance` and `timer` role (those last gates are command-level,
  not capability-level — see `references/contracts.md`).
- **admin** is bootstrap-only: project list/create, status list/next, version
  list, and `workflow bootstrap`.
- **executor** and **reviewer** share the read/comment/project/status/version/
  relation-read surface; executor alone can write to its own audit note, but
  both are barred from issue write/close/search, relation write, repo create,
  timer, and status transition.
- **tester** is comment + attachment read/write surface only: issue read,
  comment read/find/create, and attachment upload. It never sees project,
  status, version, or relation data.
- Capability-level entries above are authoritative; command-level gates such as
  `status set`, `status advance`, `timer *`, and `workflow bootstrap` are keyed
  to the role, not a capability, so they are listed in `references/contracts.md`.
