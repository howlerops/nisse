//! Records the git commit this build was made from, so every strategy version and every
//! replay can say exactly which code decided.
//!
//! `NISSE_GIT_SHA` in the environment wins (for builds without a `.git` directory, such as a
//! container image). Otherwise `git rev-parse HEAD` is used. If neither works the SHA is 40
//! zeros, which the store accepts but which matches no real commit: decisions made by such a
//! build cannot be replayed, so production builds must set the variable or build from a
//! checkout.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=NISSE_GIT_SHA");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs");

    let is_sha = |s: &str| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit());
    let from_env = std::env::var("NISSE_GIT_SHA").ok();
    let from_git = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_owned());
    let sha = from_env
        .into_iter()
        .chain(from_git)
        .find(|s| is_sha(s))
        .map_or_else(|| "0".repeat(40), |s| s.to_ascii_lowercase());
    println!("cargo:rustc-env=NISSE_GIT_SHA={sha}");
}
