# Forgejo provider

A role's stored Forgejo `api_base` plus its own token is the trust for one
normalized instance. The per-operation repository is independent: use
`--repository owner/repo` to name it, or let the current checkout's `origin`
attest it. Store the instance root and the outside-Git repository default with
`phasegent --provider forgejo admin auth setup --repository owner/repo --api-base INSTANCE_ROOT`; the
stored repository is a default only outside Git, never a credential
restriction. A Forgejo default configured through the environment, TOML, the
persisted machine-wide default or stored role configuration is selected through
the same precedence chain as any other provider, so it needs no explicit
`--provider`: explicit `--provider` > `PHASEGENT_PROVIDER` >
`PHASEGENT_DEFAULT_PROVIDER` (environment > TOML > persisted) > the role-scoped
provider > the Redmine fallback. `INSTANCE_ROOT` is the instance address
without a `/api/v1` suffix; the client appends that prefix itself. A number that
already carries a durable Forgejo task routes to its stored instance and
repository without repeating the flags. Forgejo rejects `--project-id` and
`--close-status-id`.

### Trusted instance and repository selection

- The credential endpoint is always the role's stored instance. An explicit
  `--api-base` is accepted only when it normalizes exactly to that instance;
  any other value refuses before the token is loaded or a request is sent. A
  credential with no stored instance fails closed; an explicit endpoint and
  repository cannot turn a bare token into trust.
- Repository selection precedence is: explicit `--repository`, then the current
  physical checkout's `origin` when it attests the trusted instance, then the
  stored repository default only outside Git.
- Origin is evidence, not an endpoint source. Only its normalized host and
  deployment prefix must equal the trusted instance; scheme and transport port
  may differ (for example `ssh://…:2224` or `git@host:owner/repo.git` against an
  HTTPS instance). Requests still go to the stored instance. An
  origin-derived API base is never used.
- Inside Git, a missing, unparseable, ambiguous or owner-less origin, or one
  whose host or deployment prefix does not match the trusted instance, refuses
  with guidance to pass `--repository`. Outside Git the stored repository is
  the default.
- Same-instance cross-repository selection is allowed: the Forgejo server
  enforces the token's actual repository permissions. A bound task keeps its
  immutable provider, instance, project and number; an explicit selection can
  disambiguate it but never repins it to another repository.


Each role reads its own Forgejo token from the secure `auth setup` prompt or
stdin; tokens never come from a command-line value, URL, log, or error. The
human operator provisions the token only: no status label is required, read or
written, and no label is created, deleted or migrated. Forgejo has no
`workflow bootstrap`; that provisioning surface remains Redmine-only.

Issues use the repository-local issue number. Results retain the native global
Forgejo issue id, but that id is never a CLI issue number. Branch links,
worktree leases, cleanup, and sync are scoped by `owner/repo` plus the local
number, so a same-number issue in another provider or repository is never
released or cleaned.

Supported: issue get/search/create/update/close, comments, records, project
(repository) reads, milestones as versions, the native issue states, and
scoped branch links, worktree leases, cleanup, and sync.

A Forgejo issue has exactly two native states, `Open` and `Closed`. There is no
status label, no `Resolved` state and no canonical phase graph: `status list`
returns those two entries without a request, `status next` reports the current
state and the single opposite state it may move to, and `status set`,
`status advance` and `status transition` accept `Open` and `Closed` (trimmed,
case insensitive) or the provider-local selectors `1` and `0`; any other target
refuses before a request. A close runs the dependency preflight and then
patches the native state, a reopen patches the native state without a
dependency gate, and a transition to the state an issue already carries is a
read-only no-op. Existing labels stay ordinary metadata: they never decide the
state and are never altered. The phase detail lives in the phasegent record, so
an issue stays `Open` through planning, implementation and review.

A merged Forgejo PR that carries a `Closes` marker closes the native issue at
the Git host; that is not a phasegent `issue close`, and a Redmine issue has no
such bridge. Reference a Redmine issue by full URL and never put
`Closes #<redmine-id>` in a Forgejo PR.

Not supported, rejected before any local or remote write: time tracking
(start/finish), relations, attachments, parent/subtask and planning fields,
project creation, and Forgejo-backed `workflow bootstrap`.

## Task identity and routing

A phasegent command can persist one immutable task identity for an issue:
provider + instance + project + issue number, addressed by an opaque integer
task id. Forgejo's instance is its canonical API base and its project is
`owner/repo`.

- `issue create` persists the identity once and records the current repository
  membership.
- `issue bind --provider forgejo ...` verifies the number on Forgejo before any
  local write and commits the identity, repository membership and branch task
  reference together. A branch link that already belongs to another immutable
  task instance is refused, never overwritten. An ordinary bind without an
  explicit provider stays an offline link and adopts nothing.
- A numbered command resolves its route in order: the current branch task
  reference, then a unique repository membership, then the unchanged legacy
  resolution. `issue get`/`update`/`close` (batch items independently),
  `comment create`, `record create`/`list`/`get` and `status transition`
  dispatch through the resolved task. `worktree acquire` is owned by the
  durable task bound to the issue in this repository when one exists, for every
  provider and without repeating `--provider` or `--repository`: the lease
  records that exact task identity, a same-number task on another instance is a
  different scope that is never reused, and an explicit provider, endpoint or
  project selection that contradicts the task refuses before any lease write or
  taxi pass. Without a bound task, a resolved Forgejo selection — explicit or
  configured — records the attested `owner/repo` scope, and every other
  selection keeps its legacy numeric lease. `timer start`,
  `timer finish` and `timer recover` apply the timer route described below
  (`timer list`/`get` are local ledger reads). `issue create`, search, project
  and version reads keep the configured path.
- An explicit `--provider`, `--api-base`, `--repository` or `--project-id`
  selects and disambiguates among several memberships. It never remaps a bound
  task to another provider, project or instance, and several memberships without
  a matching selection are an actionable ambiguity error.
- A bound task is dispatched through its stored instance and project. The role
  token is used only while the configured credential endpoint still attests that
  instance, so a changed default never repins the task and a mismatched endpoint
  refuses before any request.
- A stored or environment Forgejo default is admitted for a number without a
  durable task exactly like an explicit selection, using the same repository
  selection: explicit `--repository`, else the checkout's attested origin, else
  the stored default outside Git. Pre-existing `NULL` task references are never
  guessed or backfilled, and no host or tool selects a provider.

## Task lifecycle and limitations

`issue close` releases only the exact task leases of the closed task for every
provider and reads every other associated task through its own pinned task; a
directory is kept while any associated task is open, ambiguous or unreadable.
`issue sync` and `worktree prune` resolve each candidate issue through its
durable task membership and pin the provider and instance per task, with the
configured-provider resolution for a legacy number without a durable task.

Current limits: a branch link is unique by repository, branch, provider, project
and issue number and carries no instance, so two instances of the same provider,
project and number cannot both own one branch link and the adoption path refuses
to overwrite such a link. In a repository without an origin the repository key
falls back to the checkout root, so a linked worktree cannot see the main
checkout's task memberships; bind or adopt inside that checkout, or select the
provider explicitly. A close restores the main checkout only for an unambiguous
provider/project/number task set; several same-scope tasks skip restoration with
a bounded warning.

The timer ledger stores no task id, so a run stays numeric. A Forgejo-bound task
has no time tracking and refuses before any ledger write or projection. A
Local-bound run stays local and is never projected remotely. A Redmine-bound run
is pinned to its own instance and project and refuses when the configured
endpoint no longer attests that instance. The route is decided from the current
repository's task, so a task of another repository never selects the provider,
and a Local or Forgejo task there is a different provider rather than a
competing Redmine endpoint. Competing Redmine evidence still refuses: because
the ledger cannot attribute a run, durable Redmine tasks for the same number on
more than one instance or project — including one recorded in another
repository — are conflicting scopes and refuse before any write instead of
picking an endpoint. Single-instance Redmine numerics stay compatible, with no
schema, no task column and no run-to-task mapping.

Enabling Forgejo changes no Redmine or Local default, migrates no old issue, and
does not reinterpret legacy generic configuration. No legacy branch link or lease
row is backfilled with a task identity.
