//! Campaign loop, finding persistence and the byte-range shrinker.

use std::collections::BTreeMap;
use std::fs;
use std::panic::catch_unwind;
use std::path::Path;
use std::time::{Duration, Instant};

use phasegent_fuzz::{error_tag, parse_argv_bytes};

use crate::Options;
use crate::generate::{Rng, generate};

const MAX_SAVED_FINDINGS: usize = 20;
const SHRINK_BUDGET: usize = 20_000;

/// Cases per second and the outcome histogram are the coverage signal: every
/// distinct parse outcome the walker can reach in bounded time is counted.
pub(crate) fn run(options: &Options, seeds: &[Vec<u8>]) {
    fs::create_dir_all(&options.out).expect("create out dir");
    let transcript = options.out.join("findings.txt");
    let _ = fs::write(&transcript, "");
    let mut rng = Rng::new(options.seed);
    let mut covered: BTreeMap<String, u64> = BTreeMap::new();
    let mut findings = 0usize;
    let mut cases = 0u64;
    let started = Instant::now();
    let budget = Duration::from_secs(options.seconds);

    loop {
        if started.elapsed() >= budget {
            break;
        }
        if let Some(iters) = options.iters
            && cases >= iters
        {
            break;
        }
        cases += 1;
        let input = generate(&mut rng, seeds);
        let case_started = Instant::now();
        let outcome = catch_unwind(|| parse_argv_bytes(&input));
        let wall = case_started.elapsed();
        let tag = match outcome {
            Ok(Ok(tag)) => {
                *covered.entry(format!("ok {tag}")).or_default() += 1;
                continue;
            }
            Ok(Err(message)) => format!("err {}", error_tag(&message)),
            Err(_) => "PANIC".to_owned(),
        };
        *covered.entry(tag.clone()).or_default() += 1;
        if tag == "PANIC" {
            findings = report_finding(
                &options.out,
                &transcript,
                findings,
                cases,
                options.seed,
                &input,
                "panic",
                wall,
            );
        } else if wall >= Duration::from_millis(options.hang_ms) {
            findings = report_finding(
                &options.out,
                &transcript,
                findings,
                cases,
                options.seed,
                &input,
                "hang",
                wall,
            );
        }
    }

    let elapsed = started.elapsed();
    let seconds = elapsed.as_secs_f64();
    let mut top: Vec<(String, u64)> = covered.into_iter().collect();
    top.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
    let report = serde_json::json!({
        "cases": cases,
        "seconds": seconds,
        "exec_per_s": cases as f64 / seconds.max(f64::EPSILON),
        "findings": findings,
        "distinct_outcomes": top.len(),
        "outcomes": top.iter().map(|(tag, count)| serde_json::json!({"tag": tag, "cases": count})).collect::<Vec<_>>(),
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&report).expect("report json")
    );
    println!("findings transcript: {}", transcript.display());
}

/// Persist a panic/hang: shrink, write the exact bytes, append to the
/// transcript. Crashing inputs are findings, never seeds.
#[allow(clippy::too_many_arguments)]
fn report_finding(
    out: &Path,
    transcript: &Path,
    findings: usize,
    case: u64,
    seed: u32,
    input: &[u8],
    kind: &str,
    wall: Duration,
) -> usize {
    let mut line = format!(
        "{kind} case={case} seed={seed} wall_ms={} bytes={}\n  input_hex={}\n",
        wall.as_millis(),
        input.len(),
        hex(input)
    );
    let mut file = None;
    if findings < MAX_SAVED_FINDINGS {
        let minimal = shrink(input);
        if minimal.len() < input.len() {
            line.push_str(&format!(
                "  shrunk_bytes={} shrunk_hex={}\n",
                minimal.len(),
                hex(&minimal)
            ));
        }
        let path = out.join(format!("{kind}-{findings}.bin"));
        fs::write(&path, &minimal).expect("write finding");
        file = Some(path);
    }
    if let Some(path) = &file {
        line.push_str(&format!(
            "  replay=cargo run --release --manifest-path fuzz/Cargo.toml --bin argv_walk -- --replay {}\n",
            path.display()
        ));
    }
    append(transcript, &line);
    print!("{}", line);
    findings + 1
}

fn append(path: &Path, text: &str) {
    use std::io::Write as _;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open transcript");
    file.write_all(text.as_bytes()).expect("append transcript");
}

/// Byte-range shrinker: keep only the bytes that still reproduce the panic.
fn shrink(input: &[u8]) -> Vec<u8> {
    let is_crash = |candidate: &[u8]| catch_unwind(|| parse_argv_bytes(candidate)).is_err();
    let mut best = input.to_vec();
    let mut budget = SHRINK_BUDGET;
    let mut size = best.len() / 2;
    while size > 0 && budget > 0 {
        let mut index = 0;
        while index + size <= best.len() && budget > 0 {
            let mut candidate = best[..index].to_vec();
            candidate.extend_from_slice(&best[index + size..]);
            budget -= 1;
            if is_crash(&candidate) {
                best = candidate;
            } else {
                index += size;
            }
        }
        size /= 2;
    }
    best
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
