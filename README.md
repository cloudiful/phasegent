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
phasegent --role orchestrator admin auth setup

# Read from a protected file or another secure source
phasegent --role executor admin auth setup --stdin < /secure/path/token

# Select another provider and its API base explicitly
phasegent --role orchestrator --provider redmine admin auth setup \
  --stdin --api-base https://redmine.example.com

# Prepare the Redmine project and role memberships
phasegent --role admin --provider redmine admin workflow bootstrap \
  --repository OWNER/REPOSITORY
```

## Common Commands

```sh
phasegent --role orchestrator issue search --query "bug"
phasegent --role orchestrator issue get 123
phasegent --role orchestrator issue get 123 124 125
phasegent --role orchestrator comment list 123
phasegent --role orchestrator issue create \
  --title "Short title" --body "Issue details"
phasegent --role orchestrator issue update 123 --body "Updated details"
phasegent --role orchestrator issue close 123
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
covers the current `issue update` and `worktree prune` surfaces.

The OpenCode worktree adapter deployed by `phasegent plugin install` is a
generated single-file dist. Its sources are `assets/opencode/src/` plus the
`skills/phasegent/` prompts; rebuild with `bun run build:plugin` (`bun` is a
development requirement only), and never hand-edit the checked-in dist
`assets/opencode/phasegent-worktree.js` or an installed copy.

## License

Apache-2.0. See [LICENSE](LICENSE).
