# GitLab provider

A role's stored GitLab `api_base` plus its own token is the trust for one
normalized instance. Store the instance root and the outside-Git project
default with
`phasegent --provider gitlab admin auth setup --repository namespace[/subgroup]/project --api-base INSTANCE_ROOT --target-role <role>`.
The stored repository is a default only outside Git, never a credential
restriction. A GitLab default configured through the environment, TOML, the
persisted machine-wide default or stored role configuration is selected through
the same precedence chain as any other provider, so it needs no explicit
`--provider`: explicit `--provider` > `PHASEGENT_PROVIDER` >
`PHASEGENT_DEFAULT_PROVIDER` (environment > TOML > persisted) > the role-scoped
provider > the Redmine fallback.

`INSTANCE_ROOT` is the instance address without a `/api/v4` suffix; the client
appends that prefix itself. `repository` is the full project path
`namespace[/subgroup]/project`, not a single path segment, and the client
encodes the whole path as one project selector. A number that already carries a
durable GitLab task routes to its stored instance and project without repeating
the flags. GitLab rejects `--project-id` and `--close-status-id`.

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
  may differ (for example `ssh://…:2224` or `git@host:group/project.git` against
  an HTTPS instance). The origin path after the deployment prefix is the full
  GitLab project path, including any subgroups, so a deployment prefix is never
  mistaken for a namespace. Requests still go to the stored instance. An
  origin-derived API base is never used.
- Inside Git, a missing, unparseable, ambiguous or owner-less origin, or one
  whose host or deployment prefix does not match the trusted instance, refuses
  with guidance to pass `--repository`. Outside Git the stored repository is
  the default.
- Same-instance cross-project selection is allowed: the GitLab server enforces
  the token's actual project permissions. A bound task keeps its immutable
  provider, instance, project and iid; an explicit selection can disambiguate it
  but never repins it to another project.

Each role reads its own GitLab token from the secure `auth setup` prompt or
stdin; tokens never come from a command-line value, URL, log, or error. The
human operator selects the storage row with `--target-role <role>` (default
Admin) and is the only caller admitted to provisioning. GitLab has no
`workflow bootstrap`; that provisioning surface remains Redmine-only.

Issues use the project-local iid as the CLI issue number. Results retain the
native global GitLab issue id, but that id is never a CLI issue number. Branch
links, worktree directories, cleanup, and sync are scoped by full project path
plus the iid, so a same-number issue in another provider or project is never
released or cleaned.

Supported: issue get/search/create/update/close, comments, records, project
reads, milestones as versions, the native issue states, and scoped branch
links, worktree directories, cleanup, and sync.

A GitLab issue has exactly two native states, `Open` and `Closed`. There is no
status label, no `Resolved` state and no canonical phase graph: `status list`
returns those two entries without a request, `status next` reports the current
state and the single opposite state it may move to, and `status set`,
`status advance` and `status transition` accept `Open` and `Closed` (trimmed,
case insensitive) or the provider-local selectors `1` and `0`; any other target
refuses before a request. A close runs a read-back state preflight and then
patches the native state; relation dependency checks do not apply because GitLab
relations are unsupported. A reopen patches the native state without a
dependency gate, and a transition to the state an issue already carries is a
read-only no-op. Existing labels stay ordinary metadata: they never decide the
state and are never altered. The phase detail lives in the phasegent record, so
an issue stays `Open` through planning, implementation and review.

A merged GitLab MR that carries a `Closes` marker closes the native issue at
the Git host; that is not a phasegent `issue close`, and a Redmine issue has no
such bridge. Reference a Redmine issue by full URL and never put
`Closes #<redmine-id>` in a GitLab MR.

Not supported, rejected before any local or remote write: time tracking
(start/finish), relations, attachments, parent/subtask and planning fields,
project creation, and GitLab-backed `workflow bootstrap`.

## Pagination

GitLab pagination is not Forgejo pagination. Collection responses are followed
by the numeric `X-Next-Page` header: an empty header is the terminal marker, and
a full page without the header refuses because completion cannot be proven.
Repeated pages, malformed headers, a page that does not move forward, and an
empty page that still advertises a next page all fail explicitly; a collection
is never returned as a partial success. Pages are bounded.

## Search and cached state

GitLab issue search is live-only. It does not warm the shared local issue index
and never falls back to a lexical result. An explicit GitLab selection, or a
configured GitLab default whose endpoint or credential resolution fails,
returns the original live error instead of stale index rows. This keeps two
trusted instances from sharing an instance-free cached identity.

The fallback rule is provider-generic: a lexical fallback requires a known
non-GitLab provider and a proven source/project scope from the live provider or
explicit arguments. An unresolved default or missing scope returns the original
error instead of an unscoped/global query. Known non-GitLab scoped fallback and
warming behavior is unchanged.

`issue status` and `issue branches` report GitLab cached state as `unknown`
because the shared index key has no trusted-instance component. Use `issue get`
for the authoritative native state.

Legacy GitLab role rows retain their old `project_id` value untouched; the new
provider never reads or infers a project from it. A usable GitLab role requires
`admin auth setup --provider gitlab` with the trusted instance root and full
project path.

## Task identity and routing

A phasegent command can persist one immutable task identity for an issue:
provider + instance + project + issue number, addressed by an opaque integer
task id. GitLab's instance is its canonical API base and its project is the
full `namespace[/subgroup]/project` path.

- `issue create` persists the identity once and records the current repository
  membership.
- `issue bind --provider gitlab ...` verifies the iid on GitLab before any local
  write and commits the identity, repository membership and branch task
  reference together. A branch link that already belongs to another immutable
  task instance is refused, never overwritten. An ordinary bind without an
  explicit provider stays an offline link and adopts nothing.
- A numbered command resolves its route in order: the current branch task
  reference, then a unique repository membership, then the unchanged legacy
  resolution. `issue get`/`update`/`close` (batch items independently),
  `comment create`, `record create`/`list`/`get` and `status transition`
  dispatch through the resolved task. `worktree acquire` is owned by the
  durable task bound to the issue in this repository when one exists, for every
  provider and without repeating `--provider` or `--repository`.
- An explicit `--provider`, `--api-base`, `--repository` or `--project-id`
  selects and disambiguates among several memberships. It never remaps a bound
  task to another provider, project or instance, and several memberships without
  a matching selection are an actionable ambiguity error.
- A bound task is dispatched through its stored instance and project. The role
  token is used only while the configured credential endpoint still attests that
  instance, so a changed default never repins the task and a mismatched endpoint
  refuses before any request.

## Task lifecycle and limitations

The timer ledger stores no task id, so a run stays numeric. A GitLab-bound task
has no time tracking and refuses before any ledger write or projection. A
Local-bound run stays local and is never projected remotely. A Redmine-bound run
is pinned to its own instance and project and refuses when the configured
endpoint no longer attests that instance.

Enabling GitLab changes no Redmine, Forgejo or Local default, migrates no old
issue, and does not reinterpret legacy generic configuration. No legacy branch
link or directory row is backfilled with a task identity.
