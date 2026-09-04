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

For Redmine, an administrator can prepare the project and role memberships:

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
specified.

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

Successful commands return compact JSON. Errors are written to stderr and use
a non-zero exit status.

## License

Apache-2.0. See [LICENSE](LICENSE).
