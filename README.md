# phasegent

[简体中文](README.zh-CN.md)

`phasegent` is a role-aware CLI for provider-backed OpenCode workflows. It
provides one command-line interface for issue tracking, structured agent records,
comments, and workflow automation.

## Install

Requirements: Rust stable and Cargo.

```sh
git clone https://forgejo.cloud1ful.com/tools/phasegent.git
cd phasegent
cargo install --path . --force --features postgres,notify-dingtalk,notify-email
```

A checkout build (`cargo install --path .`) activates the local sources; a
release asset activates the tagged binary; `phasegent plugin install` activates
the OpenCode worktree adapter; `phasegent skill install` deploys the single
`phasegent` skill. Do not mix them for one verification.

On Windows x64, install the matching `phasegent-<tag>-x86_64-pc-windows-msvc.msi`
release asset. It installs for the current user under
`%LOCALAPPDATA%\Programs\phasegent-cli` and adds that folder to the user PATH;
open a new terminal to use `phasegent`. Uninstall it from Windows Settings > Apps
> Installed apps. Your user configuration and credentials are kept. Windows may
warn that the MSI is unsigned.

## Quick Start

Configure a credential for each role that will use the CLI. Credentials are
read through a secure prompt or stdin and are never accepted as command-line
values.

```sh
# Secure prompt
phasegent admin auth setup

# Read from a protected file or another secure source
phasegent admin auth setup --stdin < /secure/path/token

# Select another provider and its API base explicitly
phasegent --provider redmine admin auth setup \
  --stdin --api-base https://redmine.example.com

# Store the credential for one agent role instead of the Admin default
phasegent admin auth setup --provider redmine --stdin --api-base https://redmine.example.com --target-role executor < /secure/path/executor-key

# Prepare the Redmine project and role memberships
phasegent --provider redmine admin workflow bootstrap \
  --repository OWNER/REPOSITORY
```

The Redmine REST address is machine-wide: `auth setup --api-base` and
`admin config set redmine-api-base <URL>` store the one address every role
shares, and `config show` reports it once under `global_settings`. Credentials,
provisioned identities, provider selection, and the Redmine close-status id
stay role-scoped.

`workflow bootstrap` provisions one Redmine service user per agent role —
orchestrator, executor, reviewer, and explore — reads each one's API key
through the admin API, stores the keys in SQLite, and reconciles their direct
project memberships (Maintainer, Developer, Reporter, Reporter). It is
idempotent: rerunning it reuses every identity it already holds and creates
only the accounts that are missing.

`admin workflow bootstrap --all-projects` reconciles those four users on every
existing Redmine project instead of the current repository's project. It needs
no Git repository, enumerates the complete project set before any write,
provisions the shared identities once, and adds only missing memberships while
preserving roles already attached. Add `--dry-run` for a read-only preview that
plans the same outcome without creating users, fetching user API keys, or
writing memberships or settings. Both modes print a complete JSON report and exit `0`
when every project is ready, `1` on any blocked, warned, or errored project.
The modes may still open the local SQLite store, which can create the schema or
database file when it is absent.

Optional native Git hooks: run `phasegent hooks install` in a checkout. See
[Git hooks](docs/git-hooks.md) for message handling, fixed comment markers, and
safe update/recovery instructions.

## Common Commands

```sh
phasegent issue search --query "bug"
phasegent issue get 123
phasegent issue get 123 124 125
phasegent comment list 123
phasegent record list 123
phasegent record get 123 42
phasegent issue create \
  --title "Short title" --body "Issue details"
phasegent issue update 123 --body "Updated details"
phasegent issue close 123
phasegent issue status
phasegent issue branches 123
phasegent doctor
```

Branch links are local-only and read-only: `issue status` shows the current branch with its linked issues and cached state, and `issue branches N` lists every branch linked to that issue across all scopes in this repository.

## Output

Every command writes its result on stdout and diagnostics on stderr, in one of
two formats selected by `--output auto|text|json` (default `auto`): `auto` uses
human text for an interactive human terminal and the stable JSON document
everywhere else (a pipe, CI, or any agent role). `--no-progress` disables the
stderr spinner without changing warnings or errors. `--help` and `--version`
always print text.

```sh
phasegent issue get 123                    # text in a terminal, JSON in a pipe
phasegent --output text issue get 123      # force the human view
phasegent --output json issue get 123      # force the machine document
```

Supported collection lists return page 1 with up to 50 items in text and JSON.
Use `--page` and `--limit` to choose another page; JSON results share a page
envelope, for example `phasegent worktree list --page 2 --limit 50`. The
OpenCode adapter follows all directory pages when it needs complete collections.
The legacy `--format`/`--json` flags on documented commands still work. See
[output modes](docs/cli-output.md), [pagination](docs/cli-pagination.md), and
[worktree maintenance](docs/worktree-maintenance.md).

Use `--provider redmine`, `--provider local`, `--provider forgejo`, or
`--provider gitlab` on a command when the selected provider is not the default.
Use `--project-id ID` to override project discovery when required;
`--repository OWNER/REPOSITORY` names the Git host repository for `workflow
bootstrap` and project discovery.

Forgejo is configured like any other provider: use
`--provider forgejo --repository owner/repo --api-base INSTANCE_ROOT` to create
an issue, to adopt an existing number with `issue bind`, or for any number that
carries no durable task, and store the role token with `admin auth setup
--provider forgejo --repository owner/repo --api-base INSTANCE_ROOT
--target-role <role>` (omit `--target-role` to store the Admin row). A Forgejo
default configured through the environment, TOML, the persisted machine-wide
default or stored role configuration is selected by the shared precedence chain
(which ends with the Redmine fallback), so it needs no explicit `--provider`; it
names the repository by explicit `--repository`, else the current checkout's
origin when that attests the trusted instance, else the stored default outside
Git. `INSTANCE_ROOT` is the trusted instance address without a `/api/v1` suffix;
the client appends that prefix itself, and an explicit `--api-base` must
normalize exactly to it. A number that already carries a durable Forgejo task
routes to its stored instance and repository without repeating the flags, and a
different explicit repository never repins it. The operator provisions the
token only. Forgejo uses repository-local issue numbers
and keeps the native global id in results; it supports issues, comments,
records, repository reads, milestones as versions, the native `Open` and
`Closed` issue states, and scoped branch links, worktree directories, cleanup, and
sync. Time tracking, relations, attachments, subtasks/planning, and project
creation are unsupported. A Forgejo issue has no status label: it is `Open`
until it is closed, the phase detail lives in the record, close cleanup is
limited to the matching `owner/repo` scope, and a Redmine issue has no automatic
PR bridge. See [Forgejo provider](docs/forgejo-provider.md).

GitLab is configured the same way: use
`--provider gitlab --repository namespace[/subgroup]/project --api-base
INSTANCE_ROOT` to create an issue, to adopt an existing number with `issue
bind`, or for any number without a durable task, and store the role token with
`admin auth setup --provider gitlab --repository namespace[/subgroup]/project
--api-base INSTANCE_ROOT --target-role <role>`. `INSTANCE_ROOT` is the
instance address without a `/api/v4` suffix; the client appends that prefix
itself, and an explicit `--api-base` must normalize exactly to the role's
trusted instance. The repository is the full project path, which may include
subgroups. A durable GitLab task keeps its instance and project without
repeating the flags, and a different explicit selection never repins it. GitLab
uses project-local iids as CLI issue numbers and keeps the native global id in
results; it supports issues, comments, records, project reads, milestones as
versions, the native `Open` and `Closed` issue states, and scoped branch links,
worktree directories, cleanup, and sync. Time tracking, relations, attachments,
subtasks/planning, and project creation are unsupported. A GitLab issue has no
status label: it is `Open` until it is closed, and the phase detail lives in the
record. GitLab search is live-only and does not warm or fall back to the shared
local index; cached issue/branch state is `unknown` because the index key lacks
the trusted instance. Legacy GitLab `project_id` values are preserved but never
read. See [GitLab provider](docs/gitlab-provider.md).

Ordinary human CLI needs no agent role: with no verified host broker context and
no managed marker it runs as Operator with the admin credential resolved
centrally. An agent role is verified against the local host broker and is never
selected by an environment variable; managed sessions without a verified role
fail closed before any credential or network access. `PHASEGENT_ROLE` is a
legacy marker only and grants no identity. Provisioning a credential for a
specific agent role is a human write: `admin auth setup --target-role <role>`
selects the storage row without changing who the caller is.

Authorized `issue create` may run internal auto-bootstrap as system preparation
with the admin credential (one match or explicit `--project-id` skips it;
multiple matches fail before writes; no fallback, no caller admin authority).
Operator cannot forge agent records: `record create` is denied, use
`comment create` instead. See [docs/cli-caller-context.md](docs/cli-caller-context.md).

Provisioning (`auth setup`, config writes, `workflow bootstrap`) lives under
the human-operator `admin` group (`phasegent admin ...`) and is never invoked
by AI roles.

Run `phasegent --help` (or `phasegent --help <topic>`) for the full command
reference. `phasegent skill install` deploys the single `phasegent` skill into
the agent home skills root (`~/.agents/skills`; `--project` for the current
checkout; `--path DIR` for an explicit root), where the host's filesystem skill
discovery finds it. It picks
the tracking mode (`INLINE` / `TRACKED_ISSUE` / `LOCAL_ISSUE`), resolves the
provider from configuration through `--provider` with a Redmine fallback, and
covers the current `issue update`, structured `record` publication, `worktree
acquire --base`, `worktree probe`, and `worktree prune` surfaces.

## Records

`record create` / `record get` / `record list` are the documented agent path for
structured notes: the CLI generates one versioned metadata header and the agent
supplies only metadata plus a plain body, so a child never writes a header by
hand. A record's id is its native reference (`#change-<id>` on Redmine,
`#note-<id>` locally), and `record get ISSUE RECORD_ID` reads it back as the
plain body plus structured fields — so a parent cites the record id instead of
transcribing the note. The kind is bound to the session role: `executor` and
`reviewer` notes keep their status/verdict semantics, and the read-only
`explore` role may publish only an authorized recon record.

Tracking is Redmine, Forgejo, GitLab, or explicit offline local; an unsupported
provider name is rejected with an actionable config error before any network
call. See
[docs/agent-records.md](docs/agent-records.md) for the reference protocol, the
stable request key, and the Redmine/local boundaries.

## Worktrees

Worktree directories are recorded in a registry keyed by repository identity,
canonical path, branch, durable task or legacy issue identity, and observed
sessions. A directory can reference many tasks and sessions. The registry keeps
no lease status, heartbeat, expiration or release reason: live ownership comes
from fresh host occupancy and from whether an associated task is still open.

`phasegent worktree acquire --issue N [--session S] [--base REF]` is
orchestrator-only. The same repository, issue and session return the existing
`directory_id` idempotently; with explicit `--isolate`, `--base REF` starts the
new worktree and `phasegent/<issue>-<short6hex>` branch from that ref instead of
`HEAD` without reusing the current checkout. `--base` without `--isolate` is
rejected and never creates a worktree. Reuse is the default: the current checkout is
reused whether it is clean, dirty, unbound or linked to a historical issue.
Fresh occupancy, or an associated task that is still open, protects a different
idle task's directory; a confirmed closed historical task does not veto reuse by
itself. An unresolved occupation of the checkout path is reported as an
`isolation` error naming the explicit `--isolate` command; a worktree is never
created without that opt-in, and `--reuse`/`--isolate` are mutually exclusive.

`phasegent worktree probe [--path PATH | --issue N [--session S]]` reports the
state of a checkout as bounded JSON: whether the path exists, is a Git work
tree, is clean/dirty/unknown, its branch and `HEAD`, whether it is the main
checkout, and the matching registry directory. `--path` and `--issue` are
mutually exclusive, `--session` narrows `--issue`, and no selector probes the
current checkout. It is read-only (orchestrator, executor, reviewer): it never
calls a provider, writes a registry row, syncs, deletes or repairs, and an
`--issue` with no matching directory returns a stable empty result instead of a
guessed path.

```sh
phasegent worktree acquire --issue 123 --base main
phasegent worktree probe --issue 123
```

A directory associated with an OpenCode session (`ses_…`) is protected until the
host confirms that session no longer runs there. `issue close` returns the idle
members of that directory's session family — the sessions its registry
associations name and the descendants that descend from them — to the
repository's main checkout through the OpenCode API
(`opencode api session.move`) before removing anything, and keeps the directory
when that cannot be confirmed. A missing associated idle session is skipped; a
host list that still names the missing id is contradictory and keeps. Only
`ApiError::SessionNotFound` is treated as absence; transport, HTTP, auth and
malformed errors fail closed. After cleanup, a successful close also restores the
captured pre-issue branch in the verified main checkout when that checkout is
clean and safe, then fast-forwards only that branch's own configured upstream.
Every unsafe, missing or conflicting state keeps the checkout untouched with a
warning; repeat `issue close` retries an incomplete upstream update and is a
no-op after success or when the branch has no configured upstream.

`phasegent worktree prune` is global and removal-by-default: without `--repo`
it cleans every repository identity the directory registry records, and
`--dryrun` (alias `--dry-run`) is the read-only preview. A directory is removed
only when it is a verified non-main, clean directory; every associated task is
confirmed closed through its own durable task or a proven legacy route; a fresh
host occupancy check reports no running or unassociated session; every present
idle session family is safely relocated to the verified main checkout; and the
operation lock plus final fresh guards hold. There is no waiting period or age
threshold. A confirmed closed historical task never keeps the directory; a
still-open associated task does. An unavailable or incomplete listing, an
unresolvable or mismatched session, broken lineage, a rejected, queued or
unconfirmed move, and an unverifiable main checkout all keep the directory.

The local guards — never the main checkout, never dirty or untracked content —
run before any host query, so a locally excluded path makes zero host or
provider calls. One verified root `.cache`-only worktree is the sole dirty
exception: `.cache` is a real root directory with no tracked paths and only `??`
entries under it, and a bounded symlink-free inventory of reliable device/inode
and high-resolution change evidence must re-read identically immediately before
the guarded cache removal; a platform without reliable identity fails closed.
The ordinary no-force `git worktree remove` follows after every applicable
local, closure and fresh host gate is re-checked. A recorded path that no longer
is proven absent has its directory registration and its owned associations reclaimed
under the operation lock; dry-run reports what would be reclaimed. An inaccessible
path or symbolic link is retained. No provider read or session move is required.
An absent main registration is a stale row like any other and is reclaimed the
same way; an existing main checkout stays excluded from candidate totals; task,
branch-link and Git history are retained. The scan reuses one running-session
snapshot and deletion rechecks fresh occupancy. A cache-bearing candidate keeps
two fresh checks around the destructive cache deletion: one after the family
relocation and before the `.cache` is deleted, and one after the deletion and
before the worktree removal. A candidate without a root `.cache` runs the
non-mutating cleanliness probe and one final fresh check instead. In both flows
no external Git process sits between the final fresh check and the removal.
Kept reason codes distinguish issue state, lookup failure, legacy provenance,
local probes and running sessions, and name the session-family, cache, path and
registry failures a fresh re-check found.

The read-only scan never moves a session; the removal stage is the only place
that may move one. Coverage is the one connected OpenCode service at that moment,
not a whole-machine or multi-host inventory, and the lock protects cooperating
phasegent operations only. Output follows the global `--output` selection: the
compact text summary with directory-based counts on a human terminal, the
machine aggregate on a pipe or with `--json`; `--verbose` adds bounded
per-directory rows. The machine aggregate also carries the measured timings of
the pass — the wall clock, the pipeline stages and the nested closure and host
calls — which overlap instead of adding up; the compact text keeps the elapsed
line only. On an interactive terminal a progress spinner shows the scan
and per-removal checks, never a directory path, and is cleared before the result.
Invocations wrapped in a transparent `timeout`, `env` or `command` still receive
the managed agent role. See
[docs/worktree-maintenance.md](docs/worktree-maintenance.md) and
[docs/opencode-api-lifecycle.md](docs/opencode-api-lifecycle.md).

The OpenCode worktree adapter deployed by `phasegent plugin install` is a
generated single-file dist. Its only source is `assets/opencode/src/`; rebuild
with `bun run build:plugin` (`bun` is a development requirement only), and
never hand-edit the checked-in dist `assets/opencode/phasegent-worktree.js` or
an installed copy. The plugin registers no embedded skill and injects only a
short routing/safety hint: the protocol and role references are one filesystem
skill installed by `phasegent skill install`. The adapter keeps
an exact per-session worktree registry: a session only ever uses the worktree
recorded under its own id, a child inherits only its parent's directory, and an
unknown session is never redirected into another session's worktree. A decided
move the host cannot verify fails the prompt instead of running the turn in an
unverified directory.

## License

Apache-2.0. See [LICENSE](LICENSE).
