//! Rebuild tracking for the `include!`d binary sources.

fn main() {
    println!("cargo:rerun-if-changed=../src");
    println!("cargo:rerun-if-changed=../Cargo.toml");
}
