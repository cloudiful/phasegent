# Host identity verification

A managed session's agent role is not read from an environment variable. The
OpenCode plugin owns the facts the host reported (the agent id of a tool call
and its session id), and the CLI verifies the calling shell against a loopback
broker before it trusts any role.

## Trust model

- The broker derives the role from the facts it stored for the live tool call;
  the verification request never carries a role.
- The CLI accepts a role only when the broker echoes the exact session id the
  caller presented. When the host also set `OPENCODE_SESSION_ID`, the reply must
  match that marker too, so a stale or mismatched broker cannot answer for
  another managed session. There is no fallback to operator or admin authority.
- An endpoint must be a loopback address. Connect and read phases are bounded
  (2 s each), and both request and reply are capped at 4 KiB. The broker admits
  at most 128 live credentials and 64 concurrent sockets; every accepted
  connection has a 2000 ms absolute deadline that stays active until the
  socket closes, including after a reply, so neither an idle, trickling, nor
  half-open client can retain a slot. A connection beyond the socket limit is
  destroyed immediately without a protocol reply, so an untracked overflow
  path cannot exist.
- The token is a strong random opaque value drawn from 32 cryptographic random
  bytes; if the runtime exposes no cryptographic source the broker refuses to
  mint instead of falling back to weak entropy. It is bound to one tool call
  and can serve several CLI invocations inside that call; it is revoked when
  the call completes or its entry expires (5-minute TTL). A missing, null, or
  blank host tool-call id receives no credential (the shell stays marked
  managed-without-role) because such a credential could not be revoked. A full
  credential table refuses a new mint without evicting a live call.
- A broker that reports an unknown agent, or the human `admin` role, is
  denied: unknown host agents never map to `admin` or `orchestrator`.
- Leaving `PHASEGENT_ROLE` set grants no identity. A genuinely unmarked
  ordinary CLI (no broker context and no managed marker) stays Operator;
  deliberate same-user marker stripping is not prevented, and host permissions
  plus credential boundaries remain authoritative.

## Protocol

Versioned newline-delimited JSON over one TCP connection per CLI invocation:

```text
-> {"v":1,"op":"identify","token":"<opaque>","session":"<session id>"}
<- {"v":1,"ok":true,"role":"executor","session":"<session id>"}
<- {"v":1,"ok":false,"error":"unknown token"}
```

Environment passed to the managed shell:

- `PHASEGENT_BROKER_ADDR` — `127.0.0.1:<ephemeral port>`
- `PHASEGENT_BROKER_TOKEN` — the live tool call's opaque credential
- `PHASEGENT_SESSION_ID` — the real tool session

The bridge removes any inherited broker endpoint, token, or `PHASEGENT_ROLE`
claim from the fresh shell environment before it sets its own values, so a
shell never verifies against another call's facts. `PHASEGENT_ROLE` and
`OPENCODE_SESSION_ID` remain markers only: they never supply a role. A stale
or partial managed environment fails closed instead of becoming Operator.

## Visible shell mutation guard

The shell environment alone is not enough: an inline assignment in the command
text (`PHASEGENT_BROKER_ADDR=… phasegent …`), an `export`, an `unset`, a
mutating shell builtin, or an `env` override would run after the bridge set the
real values. The plugin's command funnel therefore scans the whole hooked
command — every segment, not only the phasegent segment — and refuses it before
execution when any visible assignment, `export`, `unset`, `builtin`-wrapped
mutation, `read`/`printf -v`/`mapfile`/`readarray`/`getopts` variable assignment,
`env -u`, `env -i`, or `env VAR=…` touches `PHASEGENT_BROKER_ADDR`,
`PHASEGENT_BROKER_TOKEN`, `PHASEGENT_SESSION_ID`, `OPENCODE_SESSION_ID`, or
`PHASEGENT_ROLE`. One destination matcher normalises a managed base name in a
bare operand, an indexed destination such as `unset PHASEGENT_SESSION_ID[0]`, or
an assignment (`VAR[0]=…`, `VAR+=…`) to its base variable, and a
managed-looking destination that cannot be resolved conservatively is refused.
Redirection targets are consumed as operands, so
`> /dev/null unset …` cannot hide the mutation behind a filename. The refusal is
a fixed message that names the variable class and never echoes a value or token;
no rewrite is attempted, because no rewrite can cancel an inline assignment.
Quoted arguments such as `--body "PHASEGENT_ROLE=x"`, ordinary `printf`/`read`
arguments that are not variable names, and heredoc data stay ordinary text.

This is a visible-surface guard, not a sandbox. A same-uid agent can still run
arbitrary code that builds a variable name at runtime (`python3 -c`, `sh -c`,
`eval`), can use a tool outside the hooked shell tools, or can talk to its own
fake responder directly; those paths stay outside the observation boundary and
are not claimed to be prevented. Windows PowerShell mutation shapes are not
covered by the automated tests and must not be assumed.

## Failure behavior

`record create` derives its kind from the verified role (`executor`,
`reviewer`, `explore` → recon). The orchestrator owns no record kind and is
denied; it publishes comments. A managed session without a verified role is
denied for business and permissioned commands only; the roleless utility
allowlist (help, doctor, plugin, hooks, local branch, global config) stays
available.

## Unsupported hosts

The broker depends on the host forwarding a real session id and agent id
through `tool.execute.before`, `shell.create.before`, and
`tool.execute.after`. On a host without that shell hook the bridge stays
disabled with a console warning and no identity is claimed; the CLI then fails
closed for a managed shell rather than guessing a role. If the broker cannot
start, the bridge still marks managed shells with the session and no usable
credential, so the CLI fails closed as managed-without-role instead of becoming
an ordinary Operator. Behavior on a real
host other than the exercised OpenCode hooks, and on Windows, is not covered by
the automated tests and must not be assumed.
