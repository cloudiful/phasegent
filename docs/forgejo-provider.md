# Forgejo provider

The Forgejo provider targets one explicitly configured `owner/repo` on an HTTP(S)
Forgejo instance. It is opt-in and never selected by a stored default: select it
per invocation with `--provider forgejo --repository owner/repo --api-base URL`,
or store the same values per role with
`phasegent --provider forgejo admin auth setup --repository owner/repo --api-base URL`.
Forgejo rejects `--project-id` and `--close-status-id`.

Each role reads its own Forgejo token from the secure `auth setup` prompt or
stdin; tokens never come from a command-line value, URL, log, or error. The
human operator provisions the token and, in the repository, every canonical
status label before status writes. Read commands never create labels, so a
missing label fails the write with guidance. Forgejo has no `workflow bootstrap`;
that provisioning surface remains Redmine-only.

Issues use the repository-local issue number. Results retain the native global
Forgejo issue id, but that id is never a CLI issue number. Branch links,
worktree leases, cleanup, and sync are scoped by `owner/repo` plus the local
number, so a same-number issue in another provider or repository is never
released or cleaned.

Supported: issue get/search/create/update/close, comments, records, project
(repository) reads, milestones as versions, the canonical status labels, and
scoped branch links, worktree leases, cleanup, and sync. Status writes follow
the canonical graph and native state: `Resolved` stays a native open label, and a
native close maps to `Closed` or `Cancelled` by its labels. A merged Forgejo PR
that carries a `Closes` marker closes the native issue at the Git host; that is
not a phasegent `issue close`, and a Redmine issue has no such bridge. Reference
a Redmine issue by full URL and never put `Closes #<redmine-id>` in a Forgejo PR.

Not supported, rejected before any local or remote write: time tracking
(start/finish), relations, attachments, parent/subtask and planning fields,
project creation, and Forgejo-backed `workflow bootstrap`.

Enabling Forgejo changes no Redmine or Local default, migrates no old issue, and
does not reinterpret legacy generic configuration.
