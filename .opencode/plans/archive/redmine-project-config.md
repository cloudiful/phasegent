# Redmine project discovery and CLI configuration

Tracking mode: `LOCAL_PLAN`
Tracking artifact: `.opencode/plans/redmine-project-config.md`
Provider fallback: Redmine workflow issue tracking was unavailable because the local phasegent database has no Redmine API base configured. The attempted `issue search` returned a structured configuration error, so implementation continues under this local plan.

## Goal

Make an already bootstrapped Redmine project discoverable from another checkout, and replace the awkward `config import-env` persistence workflow with explicit CLI configuration. In particular, persist `PHASEGENT_REDMINE_GIT_MIRROR_API_KEY` through a secure CLI input path without printing or accepting the secret as a command-line value.

## Background

The repository already has a Redmine `project list` provider operation and parser, but the user workflow is not clearly exposed as the way to discover project IDs for another machine. Configuration currently persists supported environment variables through `config import-env`, including the global mirror key and repository URL.

## Constraints

- Preserve the existing SQLite storage location, redacted `config show` output, provider precedence, and Redmine API behavior.
- Remove the `config import-env` command and its persistence implementation; environment variables may remain runtime overrides.
- Never accept the mirror bearer key as a positional or option value. Support secure interactive input and `--stdin`.
- Keep Redmine project listing usable without a configured project ID and document the follow-up role-scoped project-id configuration.
- Do not touch `Cargo.lock`, generated output, unrelated refactors, or baseline/user-owned paths.
- Use the existing Rust modules and error/JSON conventions.

## Acceptance criteria

- `phasegent --role <ROLE> --provider redmine project list` works without `--project-id` and returns project IDs, names, and identifiers through the existing Redmine API path.
- The project-list workflow and the role-scoped project-id configuration are documented in `README.md` and `README.zh-CN.md`.
- `config import-env` is no longer a valid command and is absent from command/help/documentation surfaces.
- Explicit `config set` support persists role-scoped settings and global settings needed by the former import flow, including the mirror key, with validation and no secret echo.
- `config set redmine-git-mirror-api-key --stdin` and secure interactive input persist the global key; direct secret values are rejected.
- Global mirror settings can be cleared explicitly; `config show` continues to report only presence/length for the key.
- Focused parser, storage/config, help, and regression tests pass, together with the repository test suite when the Rust toolchain is runnable.

## Scope

In scope:

- `src/command.rs`, `src/command/config.rs`, and config command help/routing.
- `src/cli.rs`, `src/config.rs`, and `src/config_write/**` for explicit config persistence.
- Existing project-list tests/help and Redmine configuration tests as needed.
- `README.md` and `README.zh-CN.md`.
- This plan file.

Out of scope:

- Redmine API/schema changes.
- Changes to authentication credential setup, mirror HTTP protocol, workflow bootstrap semantics, or provider precedence beyond removing import persistence.
- Changes to unrelated providers or timer lifecycle behavior.

## Baseline and path protection

Baseline HEAD at task start: `ba4ea542996b3b06d9f29bca5ab391bb1de9a561`

Baseline staged paths: none

Baseline unstaged paths: none

Baseline untracked paths: none

User-owned/baseline paths to protect: none recorded; preserve all unrelated changes if they appear.

## Ordered phases

1. Implement command model/parser/dispatch and explicit config persistence, remove import-env, and add focused tests. `DONE` after the bounded root-help repair; Rust tests remain environment-blocked.
2. Split explicit config write responsibilities into cohesive modules, then update bilingual public documentation for project discovery and CLI configuration. `DONE`.
3. Run format, focused tests, and full tests; repair only in-scope regressions. `DONE` for the implementation: `cargo fmt -- --check`, `git diff --check`, and `/home/dev/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/cargo test` pass (403 tests); the rustup proxy cargo preflight remains an environment-specific failure.
4. Independently review the complete change, create an exact-path checkpoint commit, push the current branch, and publish the release tag if all gates pass. `DONE`: review round 2 PASS, checkpoint `6c251e295609f712dbfba1d76cc5b15bb64c3597` pushed to `origin/main`, and `v0.4.0` published.

## Validation commands

- `cargo fmt -- --check`
- `cargo test`
- Focused config/project tests selected from `cargo test` output when needed.
- Before checkpoint: `git status --short`, `git diff --name-only`, `git diff --cached --name-only`, and `git diff --cached --check`.

## Dependencies and decisions

- Existing `Storage::save_global_setting`, `delete_global_setting`, role config writers, and `auth::redmine_git_mirror_api_key` are reused.
- The canonical CLI configuration interface is `config set <setting> [value|--stdin]` and `config clear <setting>`. Environment variable names and concise kebab-case aliases are accepted; output uses canonical names.
- Secret mirror-key input is interactive when no value is supplied, or read from stdin when `--stdin` is supplied. It is never printed.
- Existing `config provider get/set/clear` remains supported; generic `config set default-provider` may share its validation.

## Review history

- Initial implementation delegation: `PARTIAL`. The executor removed `config import-env`, added `config set/clear` and secure mirror-key input, and added focused tests. `cargo fmt` and `cargo fmt -- --check` passed; `cargo test` was blocked by the configured sccache credential-backend timeout. A residual root-help reference and source-scope repair remain.
- Repair delegation: `DONE`. Root help now advertises `config show, set, clear, and provider`; static scans found no user-facing `import-env` command/help remnant. `cargo fmt` and `cargo fmt -- --check` passed; Rust tests remained blocked by the same sccache timeout. The implementation review identified the 613-line `src/config_write.rs` as requiring decomposition before final review.
- Documentation pass: `DONE`. Both READMEs document Redmine project discovery with `project list`, role-scoped `config set redmine-project-id`, secure mirror-key configuration, and `config clear`; neither contains `import-env`.
- Validation pass: `DONE`. `cargo fmt -- --check`, `git diff --check`, and `/home/dev/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/cargo test --no-fail-fast` pass: 403 unit tests and all integration suites passed. The `/home/dev/.cargo/bin/cargo` rustup proxy still fails only during its rustc preflight with the old metadata error; this remains an environment-specific residual issue, not a project test failure.
- sccache diagnosis: `AWS_PROFILE` is `sccache-s3`, matching the profile in `~/.aws/credentials`; the file is mode `0600`, the process runs as `dev` with `HOME=/home/dev`, and `~/.config/sccache/config` currently has `no_credentials = false`. Anonymous probes to the configured RustFS bucket and a dummy object both returned HTTP `403`. A read-only client-side sccache probe succeeded with the profile (both with and without explicit credentials-file/region environment values), so the profile is valid; the daemon path still failed its credential startup check. No user sccache/AWS configuration was modified.
- Authentication decision: the user selected anonymous sccache. RustFS service administrator credentials in compose were preserved; the `sccache` bucket policy was applied with `rc` using a temporary local admin alias, which was removed afterward. Anonymous bucket access now returns HTTP `200` and a missing object returns `404`; sccache config is set to `no_credentials = true` with region `us-east-1`. The resulting direct stable Cargo validation completed successfully.
- ChezMoi source: `~/.config/sccache/config` is generated from `dot_config/sccache/config.tmpl` and `.chezmoitemplates/shared/sccache/config`; both source and target match the anonymous `us-east-1` configuration. The shared template carries a user working-tree modification and was preserved.
- Implementation validation: the direct stable Cargo binary ran all 403 tests successfully; sccache stats showed 116 Rust cache misses/compilations, zero cache read/write errors, and one compilation failure from the earlier diagnostic run. The rustup proxy's preflight failure is retained as residual environment risk and is not a project failure.
- Final review round 2: `PASS`. The independent reviewer reported `overall_correctness: patch is correct`, zero P0-P2 findings, and no remaining P3 findings after the targeted repairs. The reviewer independently confirmed command removal, config alias/role validation, secret handling and redaction, project-list dispatch, bilingual documentation, and the full validation results.
- Release publication: `v0.4.0` was selected from the highest valid prior tag `v0.3.2` as a backward-compatible feature release. The tag was created at checkpoint `6c251e295609f712dbfba1d76cc5b15bb64c3597` and pushed successfully with `git push origin v0.4.0`.
- Plan archival: `Review: SKIPPED` because this final edit only moves the completed local plan and records already completed checkpoint and release outcomes; no source, configuration, or behavior changed.

## Blocked questions

- None. The Redmine provider fallback is recorded above; no user decision is required to implement the local CLI behavior.

## Final status

`COMPLETE`
