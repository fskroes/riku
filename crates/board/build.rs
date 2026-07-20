use std::path::PathBuf;

fn main() {
    let dist = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("Cargo sets manifest dir"))
        .join("../../web/dist");
    println!("cargo:rerun-if-changed={}", dist.display());
    if !dist.join("index.html").is_file() {
        panic!(
            "web/dist is required to build riku: run `cd web && npm ci && npm run build` before cargo build"
        );
    }
}
