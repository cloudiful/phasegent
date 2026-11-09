# CLI caller context

Ordinary `phasegent` CLI needs no agent role. The caller context is routing
policy, not authenticated human identity; host permissions and credential
isolation remain required for actual security.

Presentation is not identity: the `--output` mode and the progress line are
resolved from this same caller context plus the terminal state, and they change
only what the CLI prints. A terminal never promotes an agent to a human, never
grants Operator authority, and never selects a credential. See
[cli-output.md](cli-output.md) for the format matrix and the progress rules.

- **Operator** — no managed marker (`PHASEGENT_SESSION_ID`, host
  `OPENCODE_SESSION_ID`, or legacy `PHASEGENT_ROLE`) and no host broker
  context. This is the ordinary human CLI. It may run all ordinary business
  and administrative commands (issue, status, relation, comment, notification,
  timer, worktree, config, explicit `admin workflow bootstrap`) with the
  existing admin credential resolved centrally. No `--authorized` flag is
  needed for Operator comments. No credential fallback across keys. When a
  role-scoped write needs a concrete scope, Operator uses the admin scope
  through the same central resolver by default; `--target-role <role>` selects a
  different storage row for `admin auth setup` and role-scoped
  `admin config set/clear`, without changing the caller's identity.
- **Agent(Role)** — a role the host broker verifies for this invocation
  (`orchestrator`, `executor`, `reviewer`, `explore`). The role is derived
  from the host facts the plugin observed, never from an environment
  variable, and keeps the existing agent-role permission matrix in
  `src/policy.rs` (`Role::allows`) exactly. `admin` is the human bootstrap role
  and is never an agent; a broker that claims it is denied. A verified agent is
  also rejected from the human provisioning surface (`admin auth setup` and
  `admin config set/clear`) before any secret or storage access, even when it
  passes `--target-role`. See
  [host-verification.md](host-verification.md).
- **ManagedWithoutRole** — a managed marker is present, or a broker context
  exists, but no role can be verified. Business and permissioned commands
  fail closed before any credential, provider, or side effect (`managed
  session requires a role …; the host did not verify this session`). The
  roleless utility allowlist stays available: help, doctor, plugin, hooks,
  local branch (`issue bind`/`unbind`/`status`), and global config surfaces.
  Inherited managed environments stay managed and fail closed if incomplete.
  No TTY detection.

## Managed shell bridge

Every supported managed `shell`/`bash` tool call carries its own independent
per-shell environment through a per-plugin-instance nonce bridge, not a
command FIFO. `tool.execute.before` rewrites the phasegent text exactly as
before, mints one broker credential for that tool call, and stores
`{ nonce, sessionID, token, toolCallId, expected }` before prepending one inert
comment line `# phasegent-context:<nonce>`. `shell.create.before` consumes only
on exact nonce plus exact command match, once, strips only its own header,
restores the exact rewritten bytes, and mutates only the fresh `event.env`
properties: `PHASEGENT_BROKER_ADDR`, `PHASEGENT_BROKER_TOKEN`, and
`PHASEGENT_SESSION_ID` (the real session). Any inherited broker endpoint,
token, or `PHASEGENT_ROLE` claim is removed first. Unknown, forged, mismatched,
expired, replayed, or overflow entries never authorize a guessed role; a lost
entry falls back to the host native marker rejection. Capacity is 64 entries
with a 5-minute TTL; eviction only deletes, never reuses or pairs. No global
`process.env` mutation and no mutable current role.

Canonical agent ids are exact after lowercasing: `orchestrator`, `executor`,
`reviewer`, `explore` only. Compound or substring lookalikes
(`task-executor`, `my-orchestrator`) are unknown: they still receive the
managed marker with no role and fail closed, never elevate. The role never
travels in the command text. When the host exposes no shell hook, the bridge
stays disabled with a console warning and only the existing rewrite path
applies, without misleading identity guarantees.

The same command funnel refuses any visible mutation of the managed variables
before execution. An inline assignment, `export`, `unset`, `env -u`, `env -i`,
or `env VAR=…` that names `PHASEGENT_BROKER_ADDR`, `PHASEGENT_BROKER_TOKEN`,
`PHASEGENT_SESSION_ID`, `OPENCODE_SESSION_ID`, or `PHASEGENT_ROLE` replaces the
whole command with one fixed refusal, because the mutation could run after the
bridge injected the real values. The scan covers every segment of the hooked
command, through quoted operands and the `timeout`/`env`/`command` wrappers, and
never echoes a value or token. Quoted arguments such as
`--body "PHASEGENT_ROLE=x"` and heredoc data are not mutations. This is a
visible-surface guard, not a sandbox: runtime-generated names, `sh -c`/`eval`,
non-hooked tools, and a same-uid fake responder remain outside it, and Windows
PowerShell shapes are unverified.

## Host marker

`OPENCODE_SESSION_ID` is set unconditionally by the host shell tool for every
managed shell invocation. It is classification-only: its presence (even
blank or non-Unicode) marks a managed shell so a missing verified role fails
closed instead of becoming Operator, but it never supplies a worktree or
session identity — `src/worktree/session.rs` keeps reading only
`PHASEGENT_SESSION_ID` (explicit `--session` wins, then that variable).
`PHASEGENT_ROLE` is a legacy marker only: a malformed value is still an
argument error, and a well-formed value marks the invocation as managed but
grants no identity. Ordinary shells without any marker remain Operator.
Markers are routing policy, not cryptographic or authenticated isolation;
intentional env stripping is not prevented and host permissions plus
credential boundaries remain authoritative.

## Heredoc limitations (issue #859 P2 regression)

Heredoc bodies (`<<` / `<<-` with quoted, unquoted, or quote-removed
delimiters, multiple queued bodies in order) are opaque data: body
apostrophes, quotes, semicolons, `phasegent` look-alikes, and role claims
never rewrite and never split commands. Real commands before and after
bodies still scope normally. Here-strings (`<<<`) are not heredocs, and
operators inside quoted strings or `#` comments are not declarations.
Unterminated heredocs and ambiguous quote shapes conservatively preserve
the affected text with no data transform; the execution-layer bridge and
host marker still fail closed when the role is unavailable. There is no
general shell interpreter dependency. End-to-end shell execution in tests
covers POSIX `sh` only; other platforms rely on the same opaque ranges
without claiming platform-specific execution.

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
