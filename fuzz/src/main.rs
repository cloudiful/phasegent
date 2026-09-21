//! Randomized local driver for the argv harness.
//!
//! Runs the same `&[u8] -> parser` entry as the libFuzzer target, on the stable
//! toolchain, with a reproducible PRNG: every finding records the seed and the
//! exact input bytes, so it is replayable without a sanitizer build. Panics are
//! caught and shrunk by [`driver`] before being written to `--out`; a hang is a
//! case that exceeds `--hang-ms`.
//!
//! Usage: see `fuzz/README.md`. `--replay <file>` re-runs one recorded input.

mod driver;
mod generate;

use std::fs;
use std::panic::catch_unwind;
use std::path::{Path, PathBuf};

use phasegent_fuzz::parse_argv_bytes;

const HANG_MILLIS_DEFAULT: u64 = 250;

fn main() {
    let options = Options::parse();
    if let Some(path) = options.replay {
        replay(&path);
        return;
    }
    let seeds = load_seeds(&options.seeds);
    if options.selftest {
        selftest(&seeds);
        return;
    }
    driver::run(&options, &seeds);
}

/// Command-line surface of the driver, shared with [`driver::run`].
pub(crate) struct Options {
    pub(crate) seconds: u64,
    pub(crate) iters: Option<u64>,
    pub(crate) seed: u32,
    pub(crate) out: PathBuf,
    pub(crate) seeds: PathBuf,
    pub(crate) hang_ms: u64,
    pub(crate) replay: Option<PathBuf>,
    pub(crate) selftest: bool,
}

impl Options {
    fn parse() -> Self {
        let mut options = Self {
            seconds: 300,
            iters: None,
            seed: 0x558,
            out: PathBuf::from("target/tmp/argv-fuzz"),
            seeds: PathBuf::from("fuzz/seeds"),
            hang_ms: HANG_MILLIS_DEFAULT,
            replay: None,
            selftest: false,
        };
        let mut args = std::env::args().skip(1);
        while let Some(flag) = args.next() {
            let mut value = || args.next().expect("flag needs a value");
            match flag.as_str() {
                "--seconds" => options.seconds = value().parse().expect("seconds"),
                "--iters" => options.iters = Some(value().parse().expect("iters")),
                "--seed" => options.seed = value().parse().expect("seed"),
                "--out" => options.out = PathBuf::from(value()),
                "--seeds" => options.seeds = PathBuf::from(value()),
                "--hang-ms" => options.hang_ms = value().parse().expect("hang-ms"),
                "--replay" => options.replay = Some(PathBuf::from(value())),
                "--selftest" => options.selftest = true,
                other => panic!("unknown flag '{other}'"),
            }
        }
        options
    }
}

/// Fixed-input smoke checks for the entry point and the corpus format, run
/// before a campaign so a broken harness never looks like a clean run.
fn selftest(seeds: &[Vec<u8>]) {
    let cases: &[(&[u8], Result<&str, &str>)] = &[
        (b"", Ok("Help/Root")),
        (b"env:\0--version", Ok("Version")),
        (b"env:executor\0frobnicate", Err("unknown command")),
        (
            b"env:not-a-role\0issue\0get\x00544",
            Err("PHASEGENT_ROLE is invalid"),
        ),
    ];
    for (input, expected) in cases {
        let actual = parse_argv_bytes(input);
        let ok = match (expected, &actual) {
            (Ok(tag), Ok(got)) => got == tag,
            (Err(prefix), Err(message)) => message.starts_with(prefix),
            _ => false,
        };
        assert!(
            ok,
            "selftest {input:?}: expected {expected:?}, got {actual:?}"
        );
    }
    for seed in seeds {
        let outcome = catch_unwind(|| parse_argv_bytes(seed));
        assert!(outcome.is_ok(), "seed must not panic: {:?}", seed);
    }
    println!("selftest ok: {} cases, {} seeds", cases.len(), seeds.len());
}

/// Re-run one recorded input and print its outcome, for evidence and triage.
fn replay(path: &Path) {
    let input = fs::read(path).expect("read replay input");
    match catch_unwind(|| parse_argv_bytes(&input)) {
        Ok(Ok(tag)) => println!("ok {tag}"),
        Ok(Err(message)) => println!("err {message}"),
        Err(_) => {
            println!("panic");
            std::process::exit(2);
        }
    }
}

fn load_seeds(dir: &Path) -> Vec<Vec<u8>> {
    let mut seeds = Vec::new();
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        if entry.path().is_file() {
            seeds.push(fs::read(entry.path()).expect("read seed"));
        }
    }
    assert!(!seeds.is_empty(), "no corpus seeds under {}", dir.display());
    seeds
}
