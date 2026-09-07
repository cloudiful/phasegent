//! Tauri build hook for the single-binary shell.
//!
//! The hook is active only when the `gui` Cargo feature is enabled so
//! ordinary CLI builds and tests never require desktop system libraries or
//! a frontend distribution directory. Release GUI builds enable the feature
//! (`cargo build --features gui`) and then embed `tauri.conf.json`, the
//! `capabilities/` manifest, and the frontend bundle via `tauri_build::build()`.
//!
//! Freshness: `tauri_build::build()` emits a `rerun-if-changed` hint for the
//! configured `frontend_dist` directory, but Cargo normalises a directory-level
//! hint to the directory path alone and does not react to content changes inside
//! it, and sccache replays cached build-script and compile units regardless. To
//! make any emitted `frontend/dist` change bust both the Cargo fingerprint and
//! sccache we write a short content hash of the dist tree to `OUT_DIR` and the
//! GUI crate includes it (`include_str!`); the hash is a real compile input, so
//! any dist change retriggers the embed.

/// FNV-1a 64-bit offset basis and prime, reproduced in `scripts/dist-hash.mjs`
/// so the operator's comparison and the embedded Status-page canary agree.
#[cfg(feature = "gui")]
const FNV_OFFSET: u64 = 0xcbf29ce484222325;
#[cfg(feature = "gui")]
const FNV_PRIME: u64 = 0x00000100000001b3;

#[cfg(feature = "gui")]
fn main() {
    let out_dir = std::env::var_os("OUT_DIR")
        .map(std::path::PathBuf::from)
        .expect("OUT_DIR must be set by cargo");
    let dist = std::path::PathBuf::from("frontend/dist");
    ensure_frontend_ready(&dist);
    tauri_build::build();
    emit_frontend_hints(&dist, &out_dir);
}

#[cfg(not(feature = "gui"))]
fn main() {
    println!("cargo:rerun-if-changed=tauri.conf.json");
    println!("cargo:rerun-if-changed=capabilities/default.json");
    println!("cargo:rerun-if-changed=frontend/index.html");
}

/// Byte-wise FNV-1a hash of the entire `frontend/dist` tree.
///
/// Files are collected recursively and sorted by forward-slash relative path;
/// each file contributes its relative path bytes, a `0x00` separator, its
/// content bytes, and a final `0x00`. Hashing the content bytes (not just
/// metadata) means any emitted bundle change alters the hash, so the crate
/// re-embeds even when file sizes and mtimes happen to match. Returns `None`
/// when the directory is absent.
#[cfg(feature = "gui")]
fn frontend_dist_hash(dist: &std::path::Path) -> Option<String> {
    let mut files = Vec::new();
    collect_files(dist, &mut files);
    if files.is_empty() {
        return None;
    }
    files.sort();
    let mut hash = FNV_OFFSET;
    for relative in &files {
        for byte in relative.as_bytes() {
            hash = fnv_step(hash, *byte);
        }
        hash = fnv_step(hash, 0x00);
        if let Ok(bytes) = std::fs::read(dist.join(relative)) {
            for byte in &bytes {
                hash = fnv_step(hash, *byte);
            }
        }
        hash = fnv_step(hash, 0x00);
    }
    Some(format!("{hash:016x}"))
}

#[cfg(feature = "gui")]
fn fnv_step(hash: u64, byte: u8) -> u64 {
    (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
}

/// Recursively collect files under `root`, storing their paths relative to
/// `root` using forward slashes so the Node mirror sorts identically.
#[cfg(feature = "gui")]
fn collect_files(root: &std::path::Path, out: &mut Vec<String>) {
    collect_files_in(root, root, out);
}

#[cfg(feature = "gui")]
fn collect_files_in(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files_in(root, &path, out);
        } else {
            let relative = path.strip_prefix(root).unwrap_or(&path);
            out.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
}

/// Emit per-file `rerun-if-changed` hints and write the content hash to
/// `OUT_DIR` so the crate (which `include_str!`s it) recompiles whenever the
/// dist tree changes. Always writes the hash file (with a sentinel when the
/// dist is absent) so the `include_str!` path never fails.
#[cfg(feature = "gui")]
fn emit_frontend_hints(dist: &std::path::Path, out_dir: &std::path::Path) {
    let hash = frontend_dist_hash(dist).unwrap_or_else(|| "unavailable".to_owned());
    let mut files = Vec::new();
    collect_files(dist, &mut files);
    for relative in files {
        println!("cargo:rerun-if-changed={}", dist.join(relative).display());
    }
    let _ = std::fs::write(out_dir.join("frontend_dist.hash"), &hash);
}

/// Auto-build the GUI frontend bundle before Tauri embeds it.
///
/// `cargo install --path .` (and every plain `cargo build/check --features
/// gui`) drives the build directly, so the Tauri-CLI `beforeBuildCommand`
/// never fires and an untracked (or stale) `frontend/dist` would silently be
/// embedded. This hook compensates: when the dist is missing or out of date
/// against the frontend sources, it runs `bun run validate` from the repo
/// root to (re)build and verify the bundle.
///
/// The check never mutates the tree when the dist is fresh and requires no
/// frontend toolchain in that case. It only ever *fails* (before
/// `tauri_build`) when it actually needs to rebuild but the toolchain is
/// missing — it never runs `bun install` or touches the network.
#[cfg(feature = "gui")]
fn ensure_frontend_ready(dist: &std::path::Path) {
    if !dist_is_stale(dist) {
        println!("build.rs: frontend dist is fresh; skipping `bun run validate`");
        return;
    }
    if !bun_available() {
        eprintln!(
            "build.rs: ERROR: frontend is missing or stale but `bun` is not on PATH; run `bun install`, then rebuild."
        );
        std::process::exit(1);
    }
    if !std::path::Path::new("frontend/node_modules").exists() {
        eprintln!(
            "build.rs: ERROR: frontend is missing or stale but `frontend/node_modules` is absent; run `bun install`, then rebuild."
        );
        std::process::exit(1);
    }
    println!("build.rs: frontend dist is missing or stale; running `bun run validate`");
    let status = std::process::Command::new("bun")
        .args(["run", "validate"])
        .current_dir(".")
        .status();
    match status {
        Ok(ok) if ok.success() => {}
        Ok(not_ok) => {
            eprintln!(
                "build.rs: ERROR: `bun run validate` failed (exit {not_ok}); the GUI build needs a fresh frontend bundle; run `bun install`, then rebuild."
            );
            std::process::exit(1);
        }
        Err(err) => {
            eprintln!(
                "build.rs: ERROR: failed to spawn `bun run validate` ({err}); run `bun install`, then rebuild."
            );
            std::process::exit(1);
        }
    }
}

/// `true` when the emitted frontend bundle is missing/empty or any frontend
/// source is newer than the newest emitted file.
///
/// Freshness is compared against the newest *emitted* file so an untouched
/// dist stays the fast path. Sources considered: every file under
/// `frontend/src`, top-level `frontend/*.ts` (excluding the generated
/// `auto-imports.d.ts`/`components.d.ts` build byproducts) and
/// `frontend/*.html`, plus `package.json` and `bun.lock`.
#[cfg(feature = "gui")]
fn dist_is_stale(dist: &std::path::Path) -> bool {
    let Some(newest_dist) = newest_mtime_under(dist) else {
        // dist missing or empty: a build is required.
        return true;
    };
    frontend_sources()
        .into_iter()
        .any(|source| mtime(&source).is_some_and(|m| m > newest_dist))
}

#[cfg(feature = "gui")]
fn bun_available() -> bool {
    std::process::Command::new("bun")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(feature = "gui")]
fn frontend_sources() -> Vec<std::path::PathBuf> {
    let mut sources = Vec::new();
    collect_source_files(&std::path::Path::new("frontend/src"), &mut sources);
    if let Ok(entries) = std::fs::read_dir("frontend") {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                let ext = path.extension().map(|e| e.to_string_lossy().into_owned());
                if matches!(ext.as_deref(), Some("ts" | "html")) && !is_generated_declaration(&path) {
                    sources.push(path);
                }
            }
        }
    }
    sources.push(std::path::PathBuf::from("package.json"));
    sources.push(std::path::PathBuf::from("bun.lock"));
    sources
}

/// `@nuxt/ui`'s Vite plugin rewrites these declaration files on every build, so
/// they are build byproducts (gitignored) rather than human-authored sources.
/// Including them as staleness checks would re-trigger `bun run validate` right
/// after a build that already wrote them (they can land a moment newer than
/// `frontend/dist`), causing a needless rebuild.
#[cfg(feature = "gui")]
fn is_generated_declaration(path: &std::path::Path) -> bool {
    matches!(
        path.file_name().and_then(|n| n.to_str()),
        Some("auto-imports.d.ts" | "components.d.ts")
    )
}

#[cfg(feature = "gui")]
fn collect_source_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_source_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// Whole-second precision on the emitted-tree side would let a same-second
/// source edit slip through as "fresh", so freshness compares full-precision
/// mtimes. Returning `None` means the path is absent or unreadable.
#[cfg(feature = "gui")]
fn mtime(path: &std::path::Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

#[cfg(feature = "gui")]
fn newest_mtime_under(root: &std::path::Path) -> Option<std::time::SystemTime> {
    let mut files = Vec::new();
    collect_files(root, &mut files);
    files
        .iter()
        .filter_map(|relative| mtime(&root.join(relative)))
        .max()
}
