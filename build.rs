//! Tauri build hook for the single-binary shell.
//!
//! The hook is active only when the `gui` Cargo feature is enabled so
//! ordinary CLI builds and tests never require desktop system
//! libraries or a frontend distribution directory. Release GUI builds
//! enable the feature (`cargo build --features gui`) and then embed
//! `tauri.conf.json`, the `capabilities/` manifest, and the
//! `frontend/` placeholder via `tauri_build::build()`.

fn main() {
    #[cfg(feature = "gui")]
    tauri_build::build();
    #[cfg(not(feature = "gui"))]
    {
        println!("cargo:rerun-if-changed=tauri.conf.json");
        println!("cargo:rerun-if-changed=capabilities/default.json");
        println!("cargo:rerun-if-changed=frontend/index.html");
    }
}
