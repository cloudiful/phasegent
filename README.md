# phasegent

[简体中文](README.zh-CN.md)

`phasegent` is a role-aware CLI for provider-backed OpenCode workflows. It
provides one command-line interface for issue tracking, repository operations,
comments, and workflow automation.

## Install

Requirements: Rust stable and Cargo.

```sh
git clone https://forgejo.cloud1ful.com/tools/phasegent.git
cd phasegent
cargo install --path .
```

Enable the optional PostgreSQL index backend when needed:

```sh
cargo install --path . --features postgres
```

## Quick Start

Configure a credential for each role that will use the CLI. Credentials are
read through a secure prompt or stdin and are never accepted as command-line
values.

```sh
# Secure prompt
PHASEGENT_ROLE=orchestrator phasegent admin auth setup

# Read from a protected file or another secure source
PHASEGENT_ROLE=executor phasegent admin auth setup --stdin < /secure/path/token

# Select another provider and its API base explicitly
PHASEGENT_ROLE=orchestrator phasegent --provider redmine admin auth setup \
  --stdin --api-base https://redmine.example.com

# Prepare the Redmine project and role memberships
PHASEGENT_ROLE=admin phasegent --provider redmine admin workflow bootstrap \
  --repository OWNER/REPOSITORY
```

The CLI resolves its role from the `PHASEGENT_ROLE` environment variable: a
managed OpenCode session exports it per invocation, and any other host sets it
in the shell. On PowerShell use `$env:PHASEGENT_ROLE='orchestrator'; phasegent
...` instead of the `NAME=value` prefix.

## Common Commands

```sh
PHASEGENT_ROLE=orchestrator phasegent issue search --query "bug"
PHASEGENT_ROLE=orchestrator phasegent issue get 123
PHASEGENT_ROLE=orchestrator phasegent issue get 123 124 125
PHASEGENT_ROLE=orchestrator phasegent comment list 123
PHASEGENT_ROLE=orchestrator phasegent issue create \
  --title "Short title" --body "Issue details"
PHASEGENT_ROLE=orchestrator phasegent issue update 123 --body "Updated details"
PHASEGENT_ROLE=orchestrator phasegent issue close 123
phasegent doctor
```

Use `--provider redmine` or `--provider gitlab` on a command when the selected
provider is not the default. Use `--repository OWNER/REPOSITORY` and
`--project-id ID` to override repository or project discovery when required.

Provisioning (`auth setup`, config writes, `workflow bootstrap`) lives under
the human-operator `admin` group (`phasegent admin ...`) and is never invoked
by AI roles.

Run `phasegent --help` (or `phasegent --help <topic>`) for the full command
reference, and see `skills/phasegent` for the OpenCode skill: it picks
the tracking mode (`INLINE` / `TRACKED_ISSUE` / `LOCAL_ISSUE`), resolves the
provider from configuration through `--provider` with a Forgejo fallback, and
covers the current `issue update`, `worktree acquire --base`, `worktree probe`,
and `worktree prune` surfaces.

## Worktrees

Leases are keyed by `(repo, issue, session)`. `phasegent worktree acquire
--issue N [--session S] [--base REF]` is orchestrator-only: the same triple
returns its existing lease first, and an explicit `--base REF` creates a new
worktree and `phasegent/<issue>-<short6hex>` branch from that ref instead of
`HEAD` without reusing the current checkout. A ref that does not resolve fails
locally before any worktree, branch, or lease is written.

`phasegent worktree probe [--path PATH | --issue N [--session S]]` reports the
state of a checkout as bounded JSON: whether the path exists, is a Git work
tree, is clean/dirty/unknown, its branch and `HEAD`, whether it is the main
checkout, and any matching lease. `--path` and `--issue` are mutually
exclusive, `--session` narrows `--issue`, and no selector probes the current
checkout. It is read-only (orchestrator, executor, reviewer): it never calls a
provider, writes a lease, syncs, deletes, or repairs, and an `--issue` with no
matching lease returns a stable empty result instead of a guessed path.

```sh
PHASEGENT_ROLE=orchestrator phasegent worktree acquire --issue 123 --base main
PHASEGENT_ROLE=executor phasegent worktree probe --issue 123
```

The OpenCode worktree adapter deployed by `phasegent plugin install` is a
generated single-file dist. Its sources are `assets/opencode/src/` plus the
`skills/phasegent/` prompts; rebuild with `bun run build:plugin` (`bun` is a
development requirement only), and never hand-edit the checked-in dist
`assets/opencode/phasegent-worktree.js` or an installed copy.

## License

Apache-2.0. See [LICENSE](LICENSE).
