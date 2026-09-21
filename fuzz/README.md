# Fuzz harness: argv walker

Local-only harness for the shell-ish argv walker in `src/command/argv.rs`
(issue #558, Phase 5). Not built by `cargo build`/`cargo test` at the
repository root, not wired into CI.

## Layout

| Path | Purpose |
| --- | --- |
| `src/lib.rs` | Harness: compiles the shipped binary source via `include!("../../src/main.rs")` and exposes the frozen `&[u8] -> parser` entry `parse_argv_bytes` (never panics) plus `error_tag`. |
| `fuzz_targets/argv_bytes.rs` | libFuzzer target (nightly, sanitizers). |
| `src/main.rs` | `argv_walk` entry: CLI flags, `--selftest`, `--replay`, seed loading. |
| `src/driver.rs` | Campaign loop, finding persistence, byte-range shrinker. |
| `src/generate.rs` | Reproducible PRNG plus structured templates, token pools, byte soup and seed mutation. |
| `seeds/` | Tracked corpus: NUL-separated argv tokens, one file per case. |
| `corpus/`, `artifacts/`, `target/` | Gitignored: libFuzzer corpus growth, crash reproducers, build output. |

Corpus format: argv tokens separated by `0x00`, exactly what `argv::parse`
receives from `main` (no program name). A trailing separator is not an empty
token and an empty file is zero tokens (`phasegent` with no arguments). An
optional first token `env:<value>` models `PHASEGENT_ROLE` and is not passed to
the parser (bare `env:` = blank).

## Commands

libFuzzer (nightly + `cargo-fuzz`, bounded):

```sh
cargo +nightly fuzz run argv_bytes fuzz/corpus/argv_bytes --features fuzz-target -- \
  -max_total_time=300 -timeout=10 -rss_limit_mb=4096
```

The tracked `seeds/` files are the committed corpus; copy them into the
libFuzzer corpus directory before the first run (the directory is gitignored):

```sh
mkdir -p fuzz/corpus/argv_bytes && cp fuzz/seeds/*.bin fuzz/corpus/argv_bytes/
```

Stable-toolchain driver (same entry, no sanitizer):

```sh
cargo run --release --manifest-path fuzz/Cargo.toml --bin argv_walk -- --selftest
cargo run --release --manifest-path fuzz/Cargo.toml --bin argv_walk -- --seconds 300 --seed 1368
cargo run --release --manifest-path fuzz/Cargo.toml --bin argv_walk -- --replay <finding>.bin
```

`--selftest` replays every tracked seed and asserts the entry never panics, so
the seeds double as regression checks; run it before trusting a clean campaign.

## Tracked seeds

| Seed | Covers |
| --- | --- |
| `empty-argv` | Root help with zero arguments. |
| `issue-get`, `issue-close-inline`, `role-env` | Global options, inline `--opt=value`, roles, batch reads. |
| `close-status-name-rejected` | Global-only option rejected outside workflow bootstrap. |
| `help-topic` | Multi-token help topic walk. |
| `quotes-and-redirect` | Quoted path and `2>&1` carried as argv data. |
| `config-set`, `config-set-value` | Secret `--stdin` and positional setting values. |
| `config-clear-global` | Top-level `config clear` redirect to the admin group. |
| `auth-setup-moved`, `workflow-bootstrap-moved` | Moved-command error paths. |
| `hooks-run`, `worktree-prune` | Two-positional hook form and lease maintenance flags. |
| `provider-two-arg` | Two-arg `--provider <valid>` global option success path. |
| `inline-escape` | Leading-dash value via `--opt=value`, embedded newline. |
| `long-token`, `invalid-utf8` | 4 KiB token and lossy UTF-8 decoding. |
| `shellish` | Shell separators/redirections as plain tokens. |

Findings land in the `--out` directory (`target/tmp/argv-fuzz/` by default) as
the exact input bytes plus a `findings.txt` transcript; a crashing input is a
P-finding for a follow-up issue, never a seed.
