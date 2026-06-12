//! Copies the font and icon files (provided by Bazel http_archives via the
//! FONT_FILES / ICON_FILES env vars) into OUT_DIR so `src/lib.rs` can embed them
//! with `include_bytes!`.

use std::{env, fs, path::Path};

fn main() {
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR is set for build scripts");
    let out_dir = Path::new(&out_dir);
    // The build script runs from the crate dir; FONT_FILES/ICON_FILES hold paths
    // relative to the Bazel exec root, so resolve them against it.
    let exec_root = env::var("EXEC_ROOT").expect("EXEC_ROOT is set");
    let exec_root = Path::new(&exec_root);
    for var in ["FONT_FILES", "ICON_FILES"] {
        println!("cargo:rerun-if-env-changed={var}");
        for rel in env::var(var).unwrap_or_default().split_whitespace() {
            let src = exec_root.join(rel);
            let name = src.file_name().expect("source path has a file name");
            fs::copy(&src, out_dir.join(name))
                .unwrap_or_else(|e| panic!("copy {}: {e}", src.display()));
        }
    }
}
