//! GHCR publish contract for the MCP image.
//!
//! Static-only guarantees for `.github/workflows/mcp-image.yml` plus
//! bilingual README alignment. No test requires registry credentials,
//! a Docker daemon, or network access: all assertions read committed
//! files.

use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_repo_file(name: &str) -> String {
    let path = manifest_dir().join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {name}: {error}"))
}

fn workflow() -> String {
    read_repo_file(".github/workflows/mcp-image.yml")
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
fn workflow_triggers_on_version_tags_only() {
    let workflow = workflow();
    // Tag-only trigger shape mirrors release.yml.
    assert_contains(&workflow, "tags:", "mcp-image trigger");
    assert!(
        workflow.contains("'v*'") || workflow.contains("\"v*\""),
        "mcp-image trigger must contain v* tag pattern"
    );
    assert_contains(&workflow, "push:", "mcp-image trigger");
    // No branch or pull-request surface: branch pushes never build/publish.
    assert_not_contains(&workflow, "branches:", "mcp-image trigger");
    assert_not_contains(&workflow, "pull_request", "mcp-image trigger");
    assert_not_contains(&workflow, "'main'", "mcp-image trigger");
    assert_not_contains(&workflow, "\"main\"", "mcp-image trigger");
}

#[test]
fn workflow_logs_in_to_ghcr_with_token_and_packages_write() {
    let workflow = workflow();
    assert_contains(&workflow, "packages: write", "mcp-image permissions");
    assert_contains(&workflow, "contents: read", "mcp-image permissions");
    assert_contains(&workflow, "docker/login-action", "mcp-image login");
    assert_contains(&workflow, "secrets.GITHUB_TOKEN", "mcp-image login");
    assert_contains(&workflow, "github.actor", "mcp-image login");
    assert_contains(&workflow, "ghcr.io", "mcp-image login");
}

#[test]
fn workflow_derives_image_path_and_pushes_version_plus_latest() {
    let workflow = workflow();
    // Runtime slug derivation, never a baked-in owner/repo.
    assert_contains(
        &workflow,
        "ghcr.io/${{ github.repository }}",
        "mcp-image image path",
    );
    assert_contains(&workflow, "github.repository", "mcp-image image path");
    // Version tag plus floating latest via docker/metadata-action, actually pushed.
    assert_contains(&workflow, "docker/metadata-action", "mcp-image tags");
    assert_contains(&workflow, "type=ref,event=tag", "mcp-image tags");
    assert_contains(&workflow, "type=raw,value=latest", "mcp-image tags");
    assert_contains(&workflow, "push: true", "mcp-image push");
    // No hardcoded image literals or credential values.
    assert_not_contains(&workflow, "OWNER/REPO", "mcp-image image path");
    assert_not_contains(&workflow, "tools/phasegent", "mcp-image image path");
    assert_not_contains(&workflow, "ghcr.io/tools", "mcp-image image path");
    assert_not_contains(&workflow, "ghcr.io/phasegent", "mcp-image image path");
}

#[test]
fn workflow_builds_native_per_arch_with_artifact_reuse_and_manifest() {
    let workflow = workflow();
    // Native per-arch matrix: runners, platforms, and Rust targets.
    assert_contains(&workflow, "ubuntu-24.04", "mcp-image runners");
    assert_contains(&workflow, "ubuntu-24.04-arm", "mcp-image runners");
    assert_contains(&workflow, "platforms:", "mcp-image platforms");
    assert_contains(&workflow, "linux/amd64", "mcp-image platforms");
    assert_contains(&workflow, "linux/arm64", "mcp-image platforms");
    assert_contains(
        &workflow,
        "x86_64-unknown-linux-gnu",
        "mcp-image targets",
    );
    assert_contains(
        &workflow,
        "aarch64-unknown-linux-gnu",
        "mcp-image targets",
    );
    // Single-arch image build per matrix row.
    assert_contains(
        &workflow,
        "matrix.platform",
        "mcp-image single-arch platforms",
    );
    assert_contains(
        &workflow.to_ascii_lowercase(),
        "buildx",
        "mcp-image builder",
    );
    assert_contains(&workflow, "setup-buildx-action", "mcp-image builder");
    assert_contains(&workflow, "docker/build-push-action", "mcp-image builder");
    // Prebuilt-artifact reuse across jobs.
    assert_contains(
        &workflow,
        "phasegent-image-input",
        "mcp-image artifacts",
    );
    assert_contains(&workflow, "upload-artifact", "mcp-image artifacts");
    assert_contains(&workflow, "download-artifact", "mcp-image artifacts");
    // Multi-arch manifest merge.
    assert_contains(&workflow, "imagetools create", "mcp-image manifest");
    // Pinned checkout, concurrency, and minimal permissions.
    assert_contains(
        &workflow,
        "ref: ${{ github.sha }}",
        "mcp-image checkout",
    );
    assert_contains(
        &workflow,
        "cancel-in-progress: false",
        "mcp-image concurrency",
    );
    assert_contains(&workflow, "contents: read", "mcp-image permissions");
    assert_contains(&workflow, "packages: write", "mcp-image permissions");
    // No QEMU Rust compile path: native runners only.
    assert_not_contains(&workflow, "setup-qemu-action", "mcp-image builder");
}

#[test]
fn workflow_uses_immutable_checkout() {
    let workflow = workflow();
    assert_contains(&workflow, "actions/checkout", "mcp-image checkout");
    assert_contains(&workflow, "github.sha", "mcp-image checkout");
    assert_contains(&workflow, "ref: ${{ github.sha }}", "mcp-image checkout");
}

fn assert_readme_container_alignment(name: &str) {
    let readme = read_repo_file(name);
    // Pull/run plus registry path placeholder.
    assert_contains(&readme, "docker pull", name);
    assert_contains(&readme, "docker run", name);
    assert_contains(&readme, "ghcr.io/", name);
    assert_contains(&readme, "OWNER/REPO", name);
    assert_contains(&readme, "v*", name);
    assert_contains(&readme, ":latest", name);
    // Token stays env-only.
    assert_contains(&readme, "PHASEGENT_MCP_AUTH_TOKEN", name);
    // Server-side role/provider flags.
    assert_contains(&readme, "--role", name);
    assert_contains(&readme, "--provider", name);
    // Storage contract.
    assert_contains(&readme, "/data", name);
    assert_contains(&readme, "PHASEGENT_DB_PATH", name);
    assert_contains(&readme, "PHASEGENT_CONFIG_PATH", name);
    // Transports and exposure warning.
    assert_contains(&readme, "127.0.0.1:3000", name);
    assert_contains(&readme.to_ascii_lowercase(), "stdio", name);
    assert_contains(&readme, "0.0.0.0", name);
}

#[test]
fn readme_container_docs_stay_aligned_en() {
    assert_readme_container_alignment("README.md");
    let readme = read_repo_file("README.md");
    assert_contains(&readme, "Warning:", "README.md");
}

#[test]
fn readme_container_docs_stay_aligned_zh() {
    assert_readme_container_alignment("README.zh-CN.md");
    let readme = read_repo_file("README.zh-CN.md");
    // Deferred P3s: pure-Chinese warning prefix and clean stdio parenthetical.
    assert_not_contains(&readme, "Warning", "README.zh-CN.md");
    assert_contains(&readme, "警告", "README.zh-CN.md");
    assert_not_contains(&readme, "stderr，stdio", "README.zh-CN.md");
    assert_not_contains(&readme, "stderr,stdio", "README.zh-CN.md");
    assert_contains(&readme, "诊断信息走 stderr", "README.zh-CN.md");
}
