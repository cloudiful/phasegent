//! Container image contract: Dockerfile/static guarantees plus an
//! optional Docker-gated build/smoke.
//!
//! The image is runtime-only: CI builds the release binary per-arch and
//! stages it at `ci-image-input/phasegent`; the Dockerfile only copies
//! that prebuilt artifact (no Rust toolchain, no `cargo build` inside
//! Docker). Static tests never require a Docker daemon or registry
//! credentials: they assert on the committed `Dockerfile`,
//! `.dockerignore`, and the bilingual container runtime docs. The single
//! build/smoke test runs only when `docker info` succeeds and the staged
//! artifact exists; otherwise it records the gap with a `SKIP` line and
//! passes so ordinary `cargo test` stays hermetic.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_repo_file(name: &str) -> String {
    let path = manifest_dir().join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {name}: {error}"))
}

fn dockerfile() -> String {
    read_repo_file("Dockerfile")
}

fn docker_available() -> bool {
    Command::new("docker")
        .arg("info")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn assert_contains(haystack: &str, needle: &str, context: &str) {
    assert!(
        haystack.contains(needle),
        "{context} must contain {needle:?}"
    );
}

fn assert_not_contains(haystack: &str, needle: &str, context: &str) {
    assert!(
        !haystack.contains(needle),
        "{context} must not contain {needle:?}"
    );
}

#[test]
fn dockerfile_is_runtime_only_copying_prebuilt_artifact() {
    let dockerfile = dockerfile();
    // Runtime-only contract: the per-arch binary is staged by CI at
    // ci-image-input/phasegent and copied into the image.
    assert_contains(
        &dockerfile,
        "ci-image-input/phasegent",
        "Dockerfile prebuilt artifact",
    );
    assert_contains(
        &dockerfile,
        "/usr/local/bin/phasegent",
        "Dockerfile install path",
    );
    let has_copy_input = dockerfile.lines().any(|line| {
        let trimmed = line.trim();
        trimmed.starts_with("COPY") && trimmed.contains("ci-image-input/phasegent")
    });
    assert!(
        has_copy_input,
        "Dockerfile must COPY ci-image-input/phasegent into the image"
    );
    // The documented CI build stays CLI-only without the `gui` feature.
    assert_contains(
        &dockerfile,
        "cargo build --release --bin phasegent",
        "Dockerfile CI build docs",
    );
    assert_contains(&dockerfile, "--no-default-features", "Dockerfile CI build docs");
    for forbidden in ["--features gui", "--features=gui", "features gui"] {
        assert_not_contains(&dockerfile, forbidden, "Dockerfile CLI-only docs");
    }
    // Runtime-only: no compile inside Docker, no Rust toolchain.
    // Comments may document the CI build command; only fail when the
    // forbidden token appears in an actual Dockerfile instruction.
    for forbidden in ["cargo build", "cargo install", "rust:", "rustup", "AS builder"] {
        let hit = dockerfile.lines().any(|line| {
            let trimmed = line.trim();
            !trimmed.starts_with('#') && line.contains(forbidden)
        });
        assert!(!hit, "Dockerfile runtime-only must not contain {forbidden:?}");
    }
    // No desktop toolchain may leak into the image build.
    for forbidden in ["tauri build", "WebKit", "libgtk", "bun run", "npm run"] {
        assert_not_contains(&dockerfile, forbidden, "Dockerfile runtime-only");
    }
    // Single runtime stage (no builder stage).
    let from_count = dockerfile
        .lines()
        .filter(|line| line.trim_start().starts_with("FROM "))
        .count();
    assert_eq!(
        from_count, 1,
        "Dockerfile must be single-stage runtime-only (found {from_count} FROM lines)"
    );
}

#[test]
fn dockerfile_runs_non_root() {
    let dockerfile = dockerfile();
    assert_contains(&dockerfile, "USER ", "Dockerfile runtime");
    assert_not_contains(&dockerfile, "USER root", "Dockerfile runtime");
    assert_not_contains(&dockerfile, "USER 0", "Dockerfile runtime");
    // A dedicated non-root user is created at build time.
    assert!(
        dockerfile.contains("useradd") || dockerfile.contains("adduser"),
        "Dockerfile must create a non-root runtime user"
    );
    assert!(
        dockerfile.contains("phasegent") && dockerfile.contains("65532"),
        "Dockerfile must run as the dedicated phasegent uid 65532"
    );
}

#[test]
fn dockerfile_has_deterministic_entrypoint_defaulting_to_loopback_http() {
    let dockerfile = dockerfile();
    // Exec-form entrypoint keeps stdio stdout protocol-clean.
    assert_contains(&dockerfile, "ENTRYPOINT [", "Dockerfile entrypoint");
    assert_contains(
        &dockerfile,
        "/usr/local/bin/phasegent",
        "Dockerfile entrypoint",
    );
    // Deterministic default: authenticated HTTP MCP on loopback.
    for token in [
        "--role",
        "mcp",
        "serve",
        "--transport",
        "http",
        "--bind",
        "127.0.0.1:3000",
    ] {
        assert_contains(&dockerfile, token, "Dockerfile default CMD");
    }
    // No shell-form entrypoint that would wrap stdout.
    for line in dockerfile.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("ENTRYPOINT ") && !trimmed.starts_with("ENTRYPOINT [") {
            panic!("ENTRYPOINT must use exec JSON form, got: {trimmed}");
        }
        if trimmed.starts_with("CMD ") && !trimmed.starts_with("CMD [") {
            panic!("CMD must use exec JSON form, got: {trimmed}");
        }
    }
}

#[test]
fn dockerfile_storage_contract_uses_volume_and_env_passthrough() {
    let dockerfile = dockerfile();
    assert_contains(&dockerfile, "VOLUME /data", "Dockerfile storage");
    assert_contains(
        &dockerfile,
        "PHASEGENT_DB_PATH=/data/",
        "Dockerfile storage",
    );
    assert_contains(
        &dockerfile,
        "PHASEGENT_CONFIG_PATH=/data/",
        "Dockerfile storage",
    );
    assert_contains(&dockerfile, "EXPOSE 3000", "Dockerfile storage");
}

#[test]
fn dockerfile_never_bakes_secrets() {
    let dockerfile = dockerfile();
    // The HTTP bearer env name is documented, never given a value.
    assert_contains(
        &dockerfile,
        "PHASEGENT_MCP_AUTH_TOKEN",
        "Dockerfile auth docs",
    );
    assert_not_contains(
        &dockerfile,
        "ENV PHASEGENT_MCP_AUTH_TOKEN=",
        "Dockerfile secrets",
    );
    assert_not_contains(
        &dockerfile,
        "ARG PHASEGENT_MCP_AUTH_TOKEN",
        "Dockerfile secrets",
    );
    assert_not_contains(&dockerfile, "Bearer ", "Dockerfile secrets");
    assert_not_contains(&dockerfile, "--stdin", "Dockerfile secrets");
    // No token files are baked into the image.
    for line in dockerfile.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("COPY") && trimmed.contains(".token") {
            panic!("Dockerfile must not COPY token files: {trimmed}");
        }
    }
}

#[test]
fn dockerfile_runtime_stays_minimal_cli_only() {
    let dockerfile = dockerfile();
    assert!(
        dockerfile.contains("bookworm-slim")
            || dockerfile.contains("distroless")
            || dockerfile.contains("alpine"),
        "Dockerfile runtime must use a minimal slim/distroless base"
    );
    assert_contains(&dockerfile, "ca-certificates", "Dockerfile runtime");
    // No GUI/frontend toolchain in the runtime image.
    for forbidden in ["tauri", "WebKit", "libgtk", "bun", "node_modules"] {
        let hit = dockerfile.lines().any(|line| {
            let trimmed = line.trim();
            !trimmed.starts_with('#') && line.contains(forbidden)
        });
        assert!(!hit, "Dockerfile runtime must not reference {forbidden:?}");
    }
}

#[test]
fn dockerignore_keeps_build_context_small() {
    let dockerignore = read_repo_file(".dockerignore");
    for required in ["target/", ".git/"] {
        assert_contains(&dockerignore, required, ".dockerignore");
    }
    // The prebuilt per-arch binary staged by CI must stay in context.
    assert_contains(&dockerignore, "!ci-image-input", ".dockerignore");
    for line in dockerignore.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('!') {
            continue;
        }
        assert!(
            !trimmed.contains("ci-image-input"),
            ".dockerignore must not exclude ci-image-input, got: {trimmed}"
        );
    }
}

fn assert_readme_container_contract(name: &str) {
    let readme = read_repo_file(name);
    // Pull/run surface.
    assert_contains(&readme, "docker pull", name);
    assert_contains(&readme, "docker run", name);
    assert_contains(&readme, "ghcr.io/", name);
    // Auth token contract: env-only, never a CLI flag value.
    assert_contains(&readme, "PHASEGENT_MCP_AUTH_TOKEN", name);
    // Server-side role/provider flags stay with the container command.
    assert_contains(&readme, "--role", name);
    assert_contains(&readme, "--provider", name);
    // Storage contract: volume plus env passthrough.
    assert_contains(&readme, "/data", name);
    assert_contains(&readme, "PHASEGENT_DB_PATH", name);
    assert_contains(&readme, "PHASEGENT_CONFIG_PATH", name);
    // Transports: loopback HTTP default plus stdio override.
    assert_contains(&readme, "127.0.0.1:3000", name);
    assert_contains(&readme.to_ascii_lowercase(), "stdio", name);
    // Security warning: loopback default, 0.0.0.0 exposure needs care
    // (English "warn" or Chinese "警告" for the localized README).
    assert!(
        readme.contains("0.0.0.0")
            && (readme.to_ascii_lowercase().contains("warn")
                || readme.contains("警告")),
        "{name} must warn about exposing HTTP beyond loopback"
    );
}

#[test]
fn readme_documents_container_runtime_contract_en() {
    assert_readme_container_contract("README.md");
}

#[test]
fn readme_documents_container_runtime_contract_zh() {
    assert_readme_container_contract("README.zh-CN.md");
}

#[test]
fn container_image_build_and_smoke_when_docker_available() {
    if !docker_available() {
        eprintln!("SKIP container_image_build_and_smoke: Docker daemon unavailable (`docker info` failed); static contract tests above still validate the Dockerfile without registry credentials");
        return;
    }
    let context = manifest_dir();
    // Runtime-only images need the CI-staged binary; without it a local
    // `docker build` cannot succeed, so record the gap and pass.
    if !context.join("ci-image-input/phasegent").is_file() {
        eprintln!("SKIP container_image_build_and_smoke: ci-image-input/phasegent not staged (CI builds it per-arch); static contract tests above still validate the runtime-only Dockerfile");
        return;
    }
    let tag = "phasegent:container-contract-test";
    let build = Command::new("docker")
        .args(["build", "-t", tag, "."])
        .current_dir(&context)
        .output()
        .expect("spawn docker build");
    assert!(
        build.status.success(),
        "docker build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let help = Command::new("docker")
        .args(["run", "--rm", tag, "--help"])
        .output()
        .expect("spawn docker smoke --help");
    assert!(
        help.status.success(),
        "container --help smoke failed: {}",
        String::from_utf8_lossy(&help.stderr)
    );
    let stdout = String::from_utf8_lossy(&help.stdout).into_owned();
    assert!(
        stdout.contains("mcp"),
        "container --help must advertise mcp; stdout={stdout}"
    );
    // Fail-closed smoke: default HTTP CMD without a bearer token must
    // exit non-zero and name the env var, never leak a secret.
    let fail_closed = Command::new("docker")
        .args([
            "run",
            "--rm",
            "--entrypoint",
            "/usr/local/bin/phasegent",
            tag,
            "--role",
            "executor",
            "mcp",
            "serve",
            "--transport",
            "http",
            "--bind",
            "127.0.0.1:3000",
        ])
        .env_remove("PHASEGENT_MCP_AUTH_TOKEN")
        .output()
        .expect("spawn docker fail-closed smoke");
    assert!(
        !fail_closed.status.success(),
        "container HTTP without token must fail closed"
    );
    let stderr = String::from_utf8_lossy(&fail_closed.stderr).into_owned();
    assert!(
        stderr.contains("PHASEGENT_MCP_AUTH_TOKEN"),
        "fail-closed stderr must name the env var; stderr={stderr}"
    );
}
