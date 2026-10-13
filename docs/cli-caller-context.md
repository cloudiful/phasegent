# CLI caller context

Ordinary `phasegent` CLI needs no agent role. The caller context is routing
policy, not authenticated human identity; host permissions and credential
isolation remain required for actual security.

- **Operator** — no `PHASEGENT_ROLE` and no `PHASEGENT_SESSION_ID`. This is the
  ordinary human CLI. It may run all ordinary business and administrative
  commands (issue, status, relation, comment, notification, timer, worktree,
  config, explicit `admin workflow bootstrap`) with the existing admin
  credential resolved centrally. No `--authorized` flag is needed for Operator
  comments. No credential fallback across keys. When a role-scoped write needs
  a concrete scope, Operator uses the admin scope through the same central
  resolver.
- **Agent(Role)** — explicit valid `PHASEGENT_ROLE` (`admin`, `orchestrator`,
  `executor`, `reviewer`, `explore`). Keeps the existing agent-role permission
  matrix in `src/policy.rs` (`Role::allows`) exactly. Invalid roles always
  error and never downgrade. Explicit `Admin` keeps its existing narrow
  permissions; Operator authority is never implemented by widening
  `Role::Admin::allows`.
- **ManagedWithoutRole** — `PHASEGENT_SESSION_ID` present (even blank) but no
  role. Business and permissioned commands fail closed before any credential,
  provider, or side effect (`managed session requires a role …; set
  PHASEGENT_ROLE`). The roleless utility allowlist stays available: help,
  doctor, plugin, hooks, local branch (`issue bind`/`unbind`/`status`), and
  global config surfaces. Inherited managed environments stay managed and fail
  closed if incomplete. No TTY detection.

## Records

Structured `record create` is agent-authored and stays role/kind-bound.
Operator gets a clear denial (`operator is not allowed to perform record
create; use comment create instead`) and uses `comment create` instead, with
no forged agent author. Record reads (`record get`/`list`) may be used by
Operator with the admin credential.

## Automatic bootstrap

Project discovery with zero matches triggers `ensure_issue_workflow` with the
existing admin credential as system preparation of an already-authorized
operation. One match uses the discovered project directly and skips bootstrap;
multiple matches fail before any write; explicit `--project-id` always wins
and skips discovery. It needs no caller role switch, `--bootstrap` flag, or
explicit admin invocation and never grants the caller admin command authority.
Managed orchestrator may reach this internal path through authorized `issue
create`; restricted roles fail before discovery. Credentials are still
required with no cross-key fallback. Direct `admin` commands stay forbidden to
AI by host policy and skills.
