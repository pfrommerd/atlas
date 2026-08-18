use std::{env, fs};

use sha2::{Digest, Sha256};

fn main() {
    publish_artifact("ATLAS_DAEMON", "CARGO_BIN_FILE_ATLAS_DAEMON_atlas-daemon");
}

fn publish_artifact(name: &str, variable: &str) {
    let path = env::var(variable)
        .unwrap_or_else(|_| panic!("Cargo did not provide the {name} binary artifact"));
    let bytes = fs::read(&path)
        .unwrap_or_else(|error| panic!("failed to read the {name} binary artifact: {error}"));
    let hash = Sha256::digest(bytes);
    println!("cargo:rustc-env={name}_ARTIFACT={path}");
    println!("cargo:rustc-env={name}_SHA256={hash:x}");
}
