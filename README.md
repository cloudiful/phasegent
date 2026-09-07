# phasegent

[简体中文](README.zh-CN.md)

`phasegent` is a role-aware CLI for provider-backed OpenCode workflows. It
provides one command-line interface for issue tracking, repository operations,
comments, and workflow automation.

## Features

- Forgejo (default), Redmine, and GitLab providers.
- Role-aware operation for `admin`, `orchestrator`, `executor`, `reviewer`, and
  `tester`.
- Issue search, creation, updates, closing, comments, statuses, relations,
  versions, and attachments where supported by the provider.
- Automatic local issue-index warming and scoped stale fallback for
  `issue search`.
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

For Redmine, an administrator can prepare the project and role memberships with only the admin API key:

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

Role credentials stay local and write-only: enter them on the Settings page
or via `auth setup`; stored keys are never displayed. Release downloads:
Windows provides one x64 MSI plus the matching raw exe; macOS provides an
unsigned Tauri app inside the `.dmg`, so Gatekeeper may warn on first open.

Windows installer shortcut options: the MSI shows a Shortcut Options page.
Start Menu is checked by default, Desktop is unchecked. Both shortcuts
start the installed app with `phasegent gui`. Silent installs keep the
defaults; override when needed:

```sh
msiexec /i phasegent-<tag>-x86_64-pc-windows-msvc.msi /qn
msiexec /i phasegent-<tag>-x86_64-pc-windows-msvc.msi /qn PHASEGENT_DESKTOP_SHORTCUT=1
msiexec /i phasegent-<tag>-x86_64-pc-windows-msvc.msi /qn PHASEGENT_STARTMENU_SHORTCUT=0
```

Upgrades keep the same per-user install identity; uninstall removes the
shortcuts, the Start Menu folder, and the user `PATH` entry.

## Configuration

`auth setup` stores provider credentials in the local configuration database.
`config show` provides a redacted view; secret values are never printed.

```sh
phasegent config show
phasegent config provider get
phasegent config provider set redmine
phasegent config provider clear
```

Provider selection can be set per command with `--provider` or for the current
environment with `PHASEGENT_PROVIDER`. Forgejo is used when no provider is
specified. See `phasegent --help config provider` for the full CLI >
environment > TOML > SQLite > defaults chain.

Redmine `workflow bootstrap` needs only the admin API key. It finds or creates
the built-in orchestrator, executor, reviewer, and tester users through the
admin API and stores their keys locally in SQLite; generated credentials stay
in SQLite and never belong in TOML.

Stable non-secret settings can be edited directly in `phasegent.toml` (default
ProjectDirs config directory, override with an absolute
`PHASEGENT_CONFIG_PATH`). Effective precedence is CLI flags > environment >
TOML > SQLite > defaults. TOML is a read-only overlay; `config set`/`clear`
and `config provider set`/`clear` continue to write SQLite, and a TOML value
shadows SQLite until removed.

Issue search uses the provider first and automatically warms the local index.
When a provider request fails, a non-empty query may use scoped stale local
results. SQLite is the default index backend. To use PostgreSQL, install with
the `postgres` feature and configure its URL through stdin:

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

## MCP server

Serve the contracted operations over the Model Context Protocol:

```sh
# Stdio (default, for local MCP clients)
phasegent --role executor mcp serve

# Streamable HTTP at /mcp
phasegent --role executor mcp serve --transport http --bind 127.0.0.1:3000
```

Tools run with the startup `--role` and provider flags; MCP clients
never supply a role. Contracted tools: `capabilities`, `issue_get`,
`issue_search`, `status_next`, `comment_create`, and `notify_send`.
`comment_create` needs server-side `--authorized` unless the server role
is orchestrator. `status_advance`, timers, and role elevation are never
exposed.

## Container image

CLI-only image (no GUI dependencies) running as non-root. The default
command serves authenticated MCP over streamable HTTP on loopback and
fails closed without a bearer token; state persists under `/data`.

```sh
docker pull ghcr.io/OWNER/REPO:latest
mkdir -p ./phasegent-data
docker run --rm -p 127.0.0.1:3000:3000 \
  -e PHASEGENT_MCP_AUTH_TOKEN="$(cat /secure/path/mcp-token)" \
  -v ./phasegent-data:/data \
  ghcr.io/OWNER/REPO:latest
```

- Token: pass `PHASEGENT_MCP_AUTH_TOKEN` with `-e` (or a secrets
  manager); never as a command argument and never baked into the image.
  HTTP without it exits before binding.
- Role/provider stay server-side: the default is `--role executor`;
  override the image CMD to change them. Clients never supply a role:
  `docker run ... ghcr.io/OWNER/REPO:latest --role executor --provider redmine mcp serve --transport http --bind 127.0.0.1:3000`
- Storage: `/data` is a volume; defaults are
  `PHASEGENT_DB_PATH=/data/phasegent.sqlite3` and
  `PHASEGENT_CONFIG_PATH=/data/phasegent.toml`. Mount
  `-v ./phasegent-data:/data` or override both paths with `-e`.
- Stdio override for local MCP clients (stdout stays protocol-clean,
  diagnostics go to stderr):
  `docker run --rm -i -v ./phasegent-data:/data ghcr.io/OWNER/REPO:latest --role executor mcp serve --transport stdio`
- Warning: the default binds loopback only. Serving with
  `--bind 0.0.0.0:3000` (plus `-p 0.0.0.0:3000:3000`) exposes
  authenticated HTTP beyond loopback: keep the bearer token secret, use
  a firewall or reverse proxy, and never publish without
  `PHASEGENT_MCP_AUTH_TOKEN` set.

Successful commands return compact JSON. Errors are written to stderr and use
a non-zero exit status.

## License

Apache-2.0. See [LICENSE](LICENSE).
