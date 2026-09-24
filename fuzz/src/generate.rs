//! Case generation: structured templates, token pools, raw byte soup and
//! seed mutation, all driven by the reproducible [`Rng`].

const COMMANDS: &[&str] = &[
    "gui", "doctor", "admin", "auth", "config", "issue", "comment", "project", "status", "version",
    "relation", "timer", "workflow", "worktree", "repo", "hooks", "plugin", "notify", "mcp",
    "help", "bogus", "",
];

const SUBCOMMANDS: &[&str] = &[
    "get",
    "search",
    "create",
    "update",
    "close",
    "sync",
    "upload-attachment",
    "bind",
    "unbind",
    "status",
    "list",
    "find-marker",
    "set",
    "clear",
    "provider",
    "next",
    "advance",
    "transition",
    "start",
    "finish",
    "recover",
    "acquire",
    "release",
    "heartbeat",
    "prune",
    "install",
    "uninstall",
    "run",
    "serve",
    "send",
    "show",
    "bootstrap",
    "prepare-commit-msg",
    "commit-msg",
    "setup",
    "delete",
];

const GLOBAL_OPTIONS: &[&str] = &[
    "--provider",
    "--api-base",
    "--repository",
    "--project-id",
    "--close-status-id",
    "--close-status-name",
    "--help",
    "-h",
    "--version",
    "-V",
];

// `--role` was removed (issue #588); the token stays in the pool so the fuzzer
// keeps exercising its rejection as an unknown option.
const INLINE_OPTIONS: &[&str] = &[
    "--role",
    "--role=orchestrator",
    "--role=",
    "--provider=Redmine",
    "--provider=",
    "--api-base=https://redmine.example",
    "--repository=owner/repo",
    "--project-id=23",
    "--project-id=",
    "--close-status-id=0",
    "--close-status-name=Closed",
    "--body=--bullet",
    "--title=-x",
    "--stdin",
    "--nonsense=1",
    "--",
];

const VALUES: &[&str] = &[
    "orchestrator",
    "executor",
    "reviewer",
    "tester",
    "admin",
    "ORCHESTRATOR",
    "nope",
    " Redmine ",
    "Redmine",
    "forgejo",
    "/tmp/x",
    "23",
    "0",
    "-1",
    "In Review",
    "-",
    "---",
    "- Goal",
    "a=b",
    "\"quoted value\"",
    "'single value'",
    "unterminated\"",
    "unterminated'",
    "中文值",
    "with\nnewline",
    "with\ttab",
    "2>&1",
];

const SHELLISH: &[&str] = &[
    "2>&1", "|", "&&", ";", "$(sub)", "`tick`", "<in", ">out", "\\", "!",
];

/// Mulberry32, the same PRNG the adapter property suite uses, so a seed names
/// the same stream across the JS and Rust fuzz tooling.
pub(crate) struct Rng(u32);

impl Rng {
    pub(crate) fn new(seed: u32) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_add(0x6D2B_79F5);
        let mut value = self.0;
        value = (value ^ (value >> 15)).wrapping_mul(value | 1);
        value ^= value.wrapping_add((value ^ (value >> 7)).wrapping_mul(value | 61));
        value ^ (value >> 14)
    }

    fn below(&mut self, limit: usize) -> usize {
        (self.next() as usize) % limit.max(1)
    }

    fn pick<'a>(&mut self, pool: &[&'a str]) -> &'a str {
        pool[self.below(pool.len())]
    }
}

/// Pick a generation strategy; the seed corpus is the mutation base, so the
/// tracked regression seeds re-enter every campaign.
pub(crate) fn generate(rng: &mut Rng, seeds: &[Vec<u8>]) -> Vec<u8> {
    match rng.below(100) {
        0..=49 => tokens_to_bytes(&template(rng)),
        50..=79 => tokens_to_bytes(&pool_case(rng)),
        80..=94 => soup(rng),
        _ => {
            let index = rng.below(seeds.len());
            mutate(rng, &seeds[index])
        }
    }
}

/// A structure-respecting case: optional role env, global options, command,
/// subcommand, then mostly option-like tail tokens.
fn template(rng: &mut Rng) -> Vec<String> {
    let mut tokens = Vec::new();
    if rng.below(4) == 0 {
        tokens.push(format!(
            "env:{}",
            if rng.below(6) == 0 {
                ""
            } else {
                rng.pick(VALUES)
            }
        ));
    }
    for _ in 0..rng.below(4) {
        if rng.below(2) == 0 {
            tokens.push(rng.pick(INLINE_OPTIONS).to_owned());
        } else {
            tokens.push(rng.pick(GLOBAL_OPTIONS).to_owned());
            tokens.push(rng.pick(VALUES).to_owned());
        }
    }
    if rng.below(10) == 0 {
        // Help form: `--help <topic> <sub> <nested>`.
        tokens.push(rng.pick(&["--help", "-h"]).to_owned());
        for _ in 0..rng.below(4) {
            tokens.push(
                rng.pick(&[
                    "issue", "close", "admin", "config", "status", "bogus", "worktree", "hooks",
                ])
                .to_owned(),
            );
        }
        return tokens;
    }
    tokens.push(rng.pick(COMMANDS).to_owned());
    for _ in 0..rng.below(4) {
        tokens.push(rng.pick(SUBCOMMANDS).to_owned());
    }
    for _ in 0..rng.below(5) {
        match rng.below(4) {
            0 => tokens.push(rng.pick(GLOBAL_OPTIONS).to_owned()),
            1 => tokens.push(rng.pick(INLINE_OPTIONS).to_owned()),
            2 => tokens.push(rng.pick(SHELLISH).to_owned()),
            _ => tokens.push(rng.pick(VALUES).to_owned()),
        }
    }
    tokens
}

fn pool_case(rng: &mut Rng) -> Vec<String> {
    let mut tokens = Vec::new();
    for _ in 0..rng.below(13) {
        let token = match rng.below(6) {
            0 => rng.pick(COMMANDS).to_owned(),
            1 => rng.pick(SUBCOMMANDS).to_owned(),
            2 => rng.pick(GLOBAL_OPTIONS).to_owned(),
            3 => rng.pick(INLINE_OPTIONS).to_owned(),
            4 => rng.pick(SHELLISH).to_owned(),
            _ => rng.pick(VALUES).to_owned(),
        };
        tokens.push(token);
    }
    tokens
}

/// Raw byte soup with invalid UTF-8 and a long-token tail.
fn soup(rng: &mut Rng) -> Vec<u8> {
    let mut input = Vec::new();
    for _ in 0..rng.below(6) {
        for _ in 0..rng.below(20) {
            let byte = (rng.next() & 0xff) as u8;
            if byte != 0 {
                input.push(byte);
            }
        }
        input.push(0);
    }
    if rng.below(4) == 0 {
        input.extend(std::iter::repeat_n(b'a', 64 + rng.below(2048)));
    }
    input
}

fn mutate(rng: &mut Rng, seed: &[u8]) -> Vec<u8> {
    let mut bytes = seed.to_vec();
    match rng.below(4) {
        0 => {
            let index = rng.below(bytes.len() + 1);
            bytes.insert(index, (rng.next() & 0xff) as u8);
        }
        1 => {
            if !bytes.is_empty() {
                let index = rng.below(bytes.len());
                bytes.remove(index);
            }
        }
        2 => bytes.extend_from_slice(&[0, (rng.next() & 0xff) as u8]),
        _ => bytes.extend_from_slice(seed),
    }
    bytes
}

fn tokens_to_bytes(tokens: &[String]) -> Vec<u8> {
    let mut input = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        if index > 0 {
            input.push(0);
        }
        input.extend_from_slice(token.as_bytes());
    }
    input
}
