# phasegent

[简体中文](README.zh-CN.md)

`phasegent` is a role-aware CLI for provider-backed OpenCode workflows. It
provides one command-line interface for issue tracking, repository operations,
comments, and workflow automation.

## Features

- Forgejo (default), Redmine, and GitLab providers.
- Roles for `admin`, `orchestrator`, `executor`, `reviewer`, and `tester`.
- Issue search, creation, updates, closing, comments, statuses, relations,
  versions, and attachments where supported by the provider.
- Local issue index (SQLite by default, optional PostgreSQL).
- Local branch-to-issue context and managed Git hooks.
- MCP server over stdio or streamable HTTP with the same role policy.
- Compact JSON output and structured errors.

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

Forgejo is the default provider. Configure a credential for each role that
will use the CLI. Credentials are read through a secure prompt or stdin and
are never accepted as command-line values.

```sh
# Secure prompt
phasegent --role orchestrator auth setup

# Read from a protected file or another secure source
phasegent --role executor auth setup --stdin < /secure/path/token
```

Select another provider and its API base explicitly:

```sh
phasegent --role orchestrator --provider redmine auth setup \
  --stdin --api-base https://redmine.example.com
```

Prepare the Redmine project and role memberships:

```sh
phasegent --role admin --provider redmine workflow bootstrap \
  --repository OWNER/REPOSITORY
```

## Common Commands

```sh
phasegent --role orchestrator issue search --query "bug"
phasegent --role orchestrator issue get 123
phasegent --role orchestrator issue create \
  --title "Short title" --body "Issue details"
phasegent --role orchestrator issue update-body 123 --body "Updated details"
phasegent --role orchestrator issue close 123
```

Use `--provider redmine` or `--provider gitlab` on a command when the selected
provider is not the default. Use `--repository OWNER/REPOSITORY` and
`--project-id ID` to override repository or project discovery when required.

Inspect the available commands with:

```sh
phasegent --help
phasegent --help issue
phasegent --help auth
```

## Desktop app

Normal `phasegent <command>` invocations stay in the CLI. Open the desktop
app explicitly with:

```sh
phasegent gui
```

Running `phasegent` with no arguments in a terminal still shows help.
Launching it from Explorer/Finder (no terminal) opens the desktop app
instead.

Release downloads: Windows provides one x64 MSI plus the matching raw exe;
macOS provides an unsigned app inside the `.dmg`, so Gatekeeper may warn on
first open.

## Configuration

`auth setup` stores provider credentials locally. `config show` provides a
redacted view; secret values are never printed.

```sh
phasegent config show
phasegent config provider get
phasegent config provider set redmine
phasegent config provider clear
```

Provider selection can be set per command with `--provider` or with
`PHASEGENT_PROVIDER`. Stable non-secret settings can be edited in
`phasegent.toml` (override its location with `PHASEGENT_CONFIG_PATH`).
Precedence is CLI flags > environment > TOML > SQLite > defaults.

To use PostgreSQL as the issue index backend, configure its URL through
stdin:

```sh
phasegent config set index-pg-url --stdin
```

## Local Branch Context

Bind an issue to the current Git branch and install managed hooks locally:

```sh
phasegent issue bind 123
phasegent issue status
phasegent hooks install
phasegent issue unbind
```

These commands operate on the local checkout and do not require provider
access.

## Worktrees

Run isolated per-issue worktrees so multiple tasks can share one repo
without colliding. Auto-isolation defaults off: on conflict, `acquire`
reuses the current checkout and warns. Enable it with
`config set worktree-auto true` or per call with `--isolate`. Copy `.env`
files by hand when the new worktree needs them.

```sh
phasegent --role orchestrator worktree acquire --issue 239 --session alpha
phasegent --role executor worktree status --issue 239
phasegent --role executor worktree list
phasegent --role orchestrator worktree release --lease lease-...
phasegent --role orchestrator worktree prune --stale-days 14 --dry-run
```

## MCP server

Serve the contracted operations over the Model Context Protocol:

```sh
# Stdio (default, for local MCP clients)
phasegent --role executor mcp serve

# Streamable HTTP at /mcp
phasegent --role executor mcp serve --transport http --bind 127.0.0.1:3000
```

Tools run with the startup `--role` and `--provider` flags; MCP clients
never supply a role.

## Notifications

Notifications are manual-only: nothing sends automatically. Send explicitly
with `notify send` (CLI) or `notify_send` (MCP):

```sh
phasegent --role executor notify send --event completion --title "Done" --body "Details"
```

Events: `completion`, `blocked`, `failure`, `interruption_suspected`,
`publish_failed`. Configure with `config set notify-enabled true` and
`config set notify-channel <name>`; secrets require `--stdin`.

## Container image

CLI-only image running as non-root. The default command serves
authenticated MCP over streamable HTTP on loopback; state persists under
`/data`. Images publish for version tags (`v*`) as `<tag>` plus `latest`.
Substitute your repository slug for `OWNER/REPO`:

```sh
docker pull ghcr.io/OWNER/REPO:latest
mkdir -p ./phasegent-data
docker run --rm -p 127.0.0.1:3000:3000 \
  -e PHASEGENT_MCP_AUTH_TOKEN="$(cat /secure/path/mcp-token)" \
  -v ./phasegent-data:/data \
  ghcr.io/OWNER/REPO:latest
```

- Token: pass `PHASEGENT_MCP_AUTH_TOKEN` with `-e`; never baked into the
  image. HTTP without it exits before binding.
- Storage: `/data` is a volume; defaults are
  `PHASEGENT_DB_PATH=/data/phasegent.sqlite3` and
  `PHASEGENT_CONFIG_PATH=/data/phasegent.toml`.
- Stdio override for local MCP clients:
  `docker run --rm -i -v ./phasegent-data:/data ghcr.io/OWNER/REPO:latest --role executor mcp serve --transport stdio`
- Warning: the default binds loopback only. Serving with `--bind 0.0.0.0:3000`
  exposes HTTP beyond loopback: keep the token secret and use a firewall
  or reverse proxy.

Successful commands return compact JSON. Errors are written to stderr and use
a non-zero exit status.

## License

Apache-2.0. See [LICENSE](LICENSE).
