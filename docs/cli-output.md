# CLI output modes

`phasegent` separates the two streams on every run:

- **stdout carries the result** — either the human text or one machine JSON
  document.
- **stderr carries diagnostics and progress** — warnings, errors, and the
  transient progress line.

The format never changes what a command does. It changes only how the same
result is presented, and it never changes the caller identity, the credentials
used, or the permissions checked.

## Selecting the format

`--output` is a global option and comes before the command name:

```sh
phasegent --output text issue get 42
phasegent --output json issue get 42
phasegent --output auto issue get 42   # the default
```

The accepted set is closed. An unknown value is an argument error before any
side effect (`--output must be 'auto', 'text', or 'json'`), a repeated
`--output` is accepted only with the same value, and a value given after the
command name is not a global option (`doctor --output json` is an argument
error).

`--no-progress` is the second global presentation switch. It disables the
progress line only; warnings and errors keep their channel and their format.

Both switches are listed on `phasegent --help`.

### The automatic default

`--output auto` resolves from the caller and the terminal state:

| Caller | stdout is a terminal, not CI | stdout is a pipe, or CI |
| --- | --- | --- |
| Operator (no role, no managed marker) | text | json |
| `PHASEGENT_ROLE=admin` | text | json |
| orchestrator, executor, reviewer, explore | json | json |
| managed session without a role | json (fails closed anyway) | json |

Only `stdout` decides the format; a piped stderr never changes it. CI counts
when `CI` or `CONTINUOUS_INTEGRATION` is set and not `0`/`false`. Terminal
detection is presentation only — it is never used as identity or authorization.

An explicit `--output text` or `--output json` always wins, for every caller
and every stream shape.

## Text

The human text keeps short labelled blocks rather than a raw dump. Curated
views exist for `issue get`, `issue search`, `issue create/update/close`,
`status list/next/set/advance`, `worktree list/status/probe/acquire/prune`,
`config show`, and `doctor`; every other command renders a readable key/value
tree.

```console
$ phasegent --output text doctor
Healthy:
 database: ~/.config/phasegent/phasegent.sqlite3

Warnings:
 default provider: not set
 issue index: sqlite is not created yet

Roles:
 admin: credential missing
 orchestrator: credential missing
 ...
```

```console
$ phasegent --output text worktree prune --dryrun
worktree prune (dry-run preview)
removed 0, eligible 0, absent 0, kept 0, removal failures 0
kept reasons: active 0, protected 0, issue-state 0, unknown 0
elapsed 0.001s
```

Text is width-aware and indented for a terminal, and long values wrap instead
of being truncated silently. A kept or failed prune directory names its reason,
so the summary stays actionable.

## JSON

`--output json` writes exactly one document on stdout, with no progress and no
diagnostic mixed in. The key sets are the stable provider-shaped documents that
predate the presentation work: this change adds no key, removes none, and
renames none. `jq` and the agent skills can rely on them.

```console
$ phasegent --output json doctor
{"database_path":"...","roles":[{"role":"admin","provider":null,...}],"global_default_provider":null,"index":{"backend":"sqlite",...}}
```

```console
$ phasegent --output json worktree prune --dryrun
{"dry_run":true,"remove":false,"release_stale":false,"stale_days":7,"repos":[],"totals":{"repositories":0,...}}
```

In `json` mode stderr carries only the structured diagnostics that already
existed. The transient progress line never appears, and no cursor control or
ANSI sequence reaches either stream.

## Errors and exit codes

A failure keeps the same information in both formats:

```console
$ phasegent --output text doctor --nonsense
error: doctor takes no arguments
```

```console
$ phasegent --output json doctor --nonsense
{"error":{"kind":"argument","message":"doctor takes no arguments"}}
```

A command-level failure adds its operation when one is known:

```console
$ phasegent --output text worktree acquire --issue 7
error: session identity is required: pass --session or set PHASEGENT_SESSION_ID
  operation: worktree acquire
```

```json
{"error":{"kind":"argument","message":"session identity is required: pass --session or set PHASEGENT_SESSION_ID","operation":"worktree acquire"}}
```

Errors go to stderr in both formats; stdout stays empty on failure. The exit
codes are unchanged: `0` success, `1` runtime failure, `2` argument,
permission, or authentication failure.

## Progress

Slow operations show an indeterminate stderr spinner while they work: the
provider requests, and the worktree list/acquire/prune and `issue sync`
operations. Its stages come from real operation boundaries, so no percentage
and no total is ever invented.

The spinner requires all of: text mode, a human caller (Operator or explicit
`admin`), a terminal on both stdout and stderr, no CI, `TERM` other than
`dumb`, and no `--no-progress`. An agent role never animates, even with
`--output text`. Help, `--version`, credential prompts, and stdin surfaces never
animate.

The line is cleared before a result or a diagnostic, so the two never overlap;
stdout is never touched. `--no-progress` disables the animation only.

## Compatibility flags

A command that always documented its own format flag keeps it:

- `worktree prune --format json|text` and the accepted `--json` no-op alias;
- `--format json` on the other commands that document it.

A legacy selection wins for that one command. It is rejected only when it
contradicts an explicit global `--output` (`worktree prune --format text
conflicts with --output json`), and `--output auto` leaves the legacy selection
in force.

### Prune default migration

`worktree prune` used to default to the compact text summary unconditionally.
It now follows the automatic default: the text summary on a human terminal, the
aggregate JSON envelope on a pipe or in CI. The stored legacy default (`text`)
still parses, so existing scripts that pass `--format text` or `--json` keep
their exact previous behavior. The machine envelope is unchanged; the text
summary now also lists kept directories with their reason.

## Help and version

`--help` and `--version` always print text, whatever `--output`, the caller, or
the stream shape says. They are the one deliberate exception: their output is
documentation, not a command result.
