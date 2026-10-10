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
phasegent admin auth setup

# Read from a protected file or another secure source
phasegent admin auth setup --stdin < /secure/path/token

# Select another provider and its API base explicitly
phasegent --provider redmine admin auth setup \
  --stdin --api-base https://redmine.example.com

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

Use `--provider redmine` or `--provider local` on a command when the selected
provider is not the default. Use `--project-id ID` to override project
discovery when required; `--repository OWNER/REPOSITORY` names the Git host
repository for `workflow bootstrap` and project discovery.

Provisioning (`auth setup`, config writes, `workflow bootstrap`) lives under
the human-operator `admin` group (`phasegent admin ...`) and is never invoked
by AI roles.

Run `phasegent --help` (or `phasegent --help <topic>`) for the full command
reference, and see `skills/phasegent` for the OpenCode skill: it picks
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

Tracking is Redmine or explicit offline local; an unsupported provider name is
rejected with an actionable config error before any network call. See
[docs/agent-records.md](docs/agent-records.md) for the reference protocol, the
stable request key, and the Redmine/local boundaries.

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
phasegent worktree acquire --issue 123 --base main
phasegent worktree probe --issue 123
```

A directory leased by an OpenCode session (`ses_…`) is protected until the host
confirms that session no longer runs there: `issue close` returns the closing
session to the repository's main checkout through the OpenCode API
(`opencode api session.move`) before removing anything, keeps the directory when
that cannot be confirmed, and `issue sync` removes it once the session has moved.
`phasegent worktree prune` is a separate, explicit removal path and is not
covered by these guards. See [docs/opencode-api-lifecycle.md](docs/opencode-api-lifecycle.md).

The OpenCode worktree adapter deployed by `phasegent plugin install` is a
generated single-file dist. Its sources are `assets/opencode/src/` plus the
`skills/phasegent/` prompts; rebuild with `bun run build:plugin` (`bun` is a
development requirement only), and never hand-edit the checked-in dist
`assets/opencode/phasegent-worktree.js` or an installed copy.

## License

Apache-2.0. See [LICENSE](LICENSE).
