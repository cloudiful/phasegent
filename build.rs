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
