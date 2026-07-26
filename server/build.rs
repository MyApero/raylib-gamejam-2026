// Admin password: reads ADMIN_PASSWORD from the environment (or from a
// gitignored .env next to this file — see .env.example) and bakes only its
// SHA-256 digest into the build via ADMIN_PASSWORD_SHA256 (consumed by
// src/lib.rs's `constants::ADMIN_PASSWORD_SHA256` through `env!`). The
// plaintext never reaches source control or the compiled module.
use sha2::{Digest, Sha256};
use std::env;
use std::path::Path;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let env_path = Path::new(&manifest_dir).join(".env");
    println!("cargo:rerun-if-changed={}", env_path.display());
    println!("cargo:rerun-if-env-changed=ADMIN_PASSWORD");

    if let Ok(contents) = std::fs::read_to_string(&env_path) {
        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                if env::var(key).is_err() {
                    env::set_var(key, value.trim());
                }
            }
        }
    }

    let password = env::var("ADMIN_PASSWORD").expect(
        "ADMIN_PASSWORD not set — copy server/.env.example to server/.env and set a password",
    );
    let hash: String = Sha256::digest(password.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    println!("cargo:rustc-env=ADMIN_PASSWORD_SHA256={hash}");
}
