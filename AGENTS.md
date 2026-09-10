# phasegent Agent Guide

This file is for AI coding agents and human contributors working in this
repository. It only covers the versioning policy and the one lock-file
exception — other conventions live in `README.md` and the docs under `docs/`.

## Versioning

The single source of truth for the release version is the git tag
`vX.Y.Z` (for example `v2.11.0`). The release workflow
(`.github/workflows/release.yml`) is tag-triggered and the Windows MSI derives
its version from the tag with strict `MAJOR.MINOR.PATCH` enforcement.

Three manifest files must always equal the tag with the leading `v`
stripped:

- `Cargo.toml` — `[package] version`
- `package.json` — top-level `version`
- `tauri.conf.json` — top-level `version`

The Rust binary's `--version` output flows from `CARGO_PKG_VERSION`, which
Cargo reads from `Cargo.toml`. Do **not** hard-code a version string in
`src/**`.

### Bump procedure

For every release (in this exact order):

1. Edit the three manifest files above to the new `X.Y.Z` and stage them in
   one commit.
2. Sync the root-package `version` in `Cargo.lock` (see the lock-file rule
   below) and stage it in the same commit. Verify with `grep` that only the
   `phasegent` entry version changed; if unrelated entries churn, restore
   the lock and stop.
3. Run `cargo fmt`, `cargo test --all-targets`, and `cargo clippy
   --all-targets -- -D warnings`. The release is only ready when all three
   are clean.
4. Tag a lightweight tag locally (`git tag vX.Y.Z`) only after double-
   checking the three manifests match the tag.
5. Push the commit, then push the tag (`git push origin vX.Y.Z`) to
   trigger `.github/workflows/release.yml`.

### `release.yml` version-check job

The workflow has a `version-check` job that runs first on `ubuntu-24.04`
with `contents: read` only. It uses Python 3 standard library
(`tomllib`, `json`) to assert that `github.ref_name` matches
`^v\d+\.\d+\.\d+$` and that all three manifest versions equal the
stripped tag. Any mismatch fails fast with a clear error message and the
`build` job never starts.

### Tag conventions

- Tags are lightweight (no `-a`/`-s`), matching the existing 36 tags.
- Tags are immutable: never re-tag, never force-push a tag.
- MSI version is strict numeric — `v1.2.3` works, `v1.2`, `v1.2.3-rc1`,
  or any pre-release suffix does not.

### Lock files

- `Cargo.lock` is normally git-ignored and is **not** committed except for
  the single one-time exception when the root package version is bumped:
  the new commit stages the updated lock alongside the three manifests.
  After that one commit the lock returns to its git-ignored state.
- `bun.lock` is git-ignored and never committed. The repository's
  no-lock policy is unchanged.
