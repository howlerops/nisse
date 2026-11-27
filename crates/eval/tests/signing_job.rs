//! Acceptance tests for `nisse_eval::signing_job`. Each test builds a small directory tree
//! under a unique temporary directory, removed when the test ends.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use nisse_core::CoreError;
use nisse_core::signing::{SigningKey, deployment_hash};
use nisse_eval::EvalError;
use nisse_eval::signing_job::{manifest_path, sign_config};
use serde_json::Value;

const CONFIG: &str = include_str!("../../../research/configs/trend-v1.json");
const NOTE: &str = include_str!("../../../research/notes/trend-v1.md");

/// A fixture key, 32 bytes of hex. Not a secret.
const KEY: &str = "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a";
const OTHER_KEY: &str = "6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b";
const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const SIGNED_AT: &str = "2026-11-27T00:00:00Z";
const CONFIG_PATH: &str = "research/configs/trend-v1.json";

/// A temporary directory tree, removed on drop.
struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new() -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("nisse-sign-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn path(&self) -> &Path {
        &self.root
    }

    fn write(&self, rel: &str, text: &str) {
        let file = self.root.join(rel);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
    }

    /// Makes `rel` a symbolic link to `target`.
    #[cfg(unix)]
    fn link(&self, rel: &str, target: &str) {
        let file = self.root.join(rel);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(target, file).unwrap();
    }

    /// A tree holding the repository's valid config and note at their usual places.
    fn valid() -> Self {
        let tree = Self::new();
        tree.write(CONFIG_PATH, CONFIG);
        tree.write("research/notes/trend-v1.md", NOTE);
        tree
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn key(text: &str) -> SigningKey {
    SigningKey::from_hex(text).unwrap()
}

/// Signs the valid tree's config with the fixture key and a clean build.
fn sign_valid(root: &Path) -> Result<nisse_core::signing::Manifest, EvalError> {
    sign_config(root, CONFIG_PATH, KEY, SHA, true, SIGNED_AT)
}

/// The error of a call that must be refused.
fn refusal<T: std::fmt::Debug>(result: Result<T, EvalError>) -> EvalError {
    match result {
        Ok(value) => panic!("expected a refusal, got {value:?}"),
        Err(e) => e,
    }
}

/// Asserts a refusal of kind `InvalidProposal` whose message contains `needle`.
fn assert_invalid_proposal<T: std::fmt::Debug>(result: Result<T, EvalError>, needle: &str) {
    match refusal(result) {
        EvalError::InvalidProposal(message) => {
            assert!(
                message.contains(needle),
                "message {message:?} does not contain {needle:?}"
            );
        }
        other => panic!("expected InvalidProposal, got {other:?}"),
    }
}

/// The `deployment_hash` of the config file, computed directly from its fields.
fn hash_of_file(text: &str) -> nisse_core::Hash32 {
    let value: Value = serde_json::from_str(text).unwrap();
    let symbols: Vec<&str> = value["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap())
        .collect();
    deployment_hash(
        &value["strategy"],
        &symbols,
        value["benchmark"].as_str().unwrap(),
    )
    .unwrap()
}

#[test]
fn signs_the_repository_config() {
    let tree = Tree::valid();
    let manifest = sign_valid(tree.path()).unwrap();

    manifest.verify(&key(KEY)).unwrap();
    assert_eq!(manifest.stage, "paper");
    assert_eq!(manifest.git_sha, SHA);
    assert_eq!(manifest.signed_at, SIGNED_AT);
    assert_eq!(manifest.name, "trend-v1");
    assert_eq!(manifest.deployment_hash, hash_of_file(CONFIG));
}

#[test]
fn the_manifest_verifies_only_under_the_same_key() {
    let tree = Tree::valid();
    let manifest = sign_valid(tree.path()).unwrap();

    manifest.verify(&key(KEY)).unwrap();
    assert_eq!(
        manifest.verify(&key(OTHER_KEY)),
        Err(CoreError::BadSignature)
    );
}

#[test]
fn refuses_a_path_outside_the_config_directory() {
    let tree = Tree::valid();
    let result = sign_config(
        tree.path(),
        "research/notes/trend-v1.md",
        KEY,
        SHA,
        true,
        SIGNED_AT,
    );
    assert_invalid_proposal(result, "must be research/configs/<name>.json");
}

#[test]
fn refuses_a_path_with_dot_dot() {
    let tree = Tree::valid();
    let result = sign_config(
        tree.path(),
        "research/configs/../configs/trend-v1.json",
        KEY,
        SHA,
        true,
        SIGNED_AT,
    );
    assert_invalid_proposal(result, "must be research/configs/<name>.json");
}

#[test]
fn refuses_a_nested_path() {
    let tree = Tree::valid();
    tree.write("research/configs/sub/trend-v1.json", CONFIG);
    let result = sign_config(
        tree.path(),
        "research/configs/sub/trend-v1.json",
        KEY,
        SHA,
        true,
        SIGNED_AT,
    );
    assert_invalid_proposal(result, "must be research/configs/<name>.json");
}

#[test]
fn refuses_an_absolute_path() {
    let tree = Tree::valid();
    let absolute = format!("/{CONFIG_PATH}");
    let result = sign_config(tree.path(), &absolute, KEY, SHA, true, SIGNED_AT);
    assert_invalid_proposal(result, "must be research/configs/<name>.json");
}

#[test]
fn refuses_a_config_that_does_not_parse() {
    let tree = Tree::new();
    tree.write(CONFIG_PATH, "{ not json");
    tree.write("research/notes/trend-v1.md", NOTE);
    let result = sign_valid(tree.path());
    assert_invalid_proposal(result, "invalid JSON");
}

#[test]
fn refuses_a_name_different_from_the_file_stem() {
    let tree = Tree::new();
    // Name and rationale both move to trend-v2, so only the file stem (trend-v1) disagrees.
    let renamed = CONFIG
        .replace("\"name\": \"trend-v1\"", "\"name\": \"trend-v2\"")
        .replace("research/notes/trend-v1.md", "research/notes/trend-v2.md");
    tree.write(CONFIG_PATH, &renamed);
    tree.write("research/notes/trend-v2.md", NOTE);
    let result = sign_valid(tree.path());
    assert_invalid_proposal(result, "must equal the file stem");
}

#[test]
fn refuses_a_missing_note() {
    let tree = Tree::new();
    tree.write(CONFIG_PATH, CONFIG);
    let result = sign_valid(tree.path());
    assert_invalid_proposal(result, "cannot read note");
}

#[test]
fn refuses_a_note_without_a_heading() {
    let tree = Tree::new();
    tree.write(CONFIG_PATH, CONFIG);
    tree.write(
        "research/notes/trend-v1.md",
        "# trend-v1\n\nSome text but no headings at all.\n",
    );
    let result = sign_valid(tree.path());
    assert_invalid_proposal(result, "missing heading");
}

#[test]
fn refuses_a_dirty_build() {
    let tree = Tree::valid();
    let result = sign_config(tree.path(), CONFIG_PATH, KEY, SHA, false, SIGNED_AT);
    assert_invalid_proposal(result, "uncommitted changes");
}

#[test]
fn refuses_an_all_zero_sha() {
    let tree = Tree::valid();
    let zeros = "0".repeat(40);
    let result = sign_config(tree.path(), CONFIG_PATH, KEY, &zeros, true, SIGNED_AT);
    assert_invalid_proposal(result, "unknown");
}

#[test]
fn refuses_a_short_key() {
    let tree = Tree::valid();
    let short = "5a".repeat(31);
    let result = sign_config(tree.path(), CONFIG_PATH, &short, SHA, true, SIGNED_AT);
    match refusal(result) {
        EvalError::Core(CoreError::InvalidSigning(message)) => {
            assert!(message.contains("at least"), "{message}");
            assert!(!message.contains(&short));
        }
        other => panic!("expected a core signing error, got {other:?}"),
    }
}

#[test]
fn refuses_a_non_hex_key_without_echoing_it() {
    let tree = Tree::valid();
    let bad = format!("zz{}", "5a".repeat(31));
    let result = sign_config(tree.path(), CONFIG_PATH, &bad, SHA, true, SIGNED_AT);
    let text = format!("{}", refusal(result));
    assert!(!text.contains(&bad), "error text leaks the key: {text}");
    assert!(!text.contains("5a5a"), "error text leaks the key: {text}");
}

#[test]
fn refuses_a_malformed_sha() {
    let tree = Tree::valid();
    let upper = SHA.to_uppercase();
    let result = sign_config(tree.path(), CONFIG_PATH, KEY, &upper, true, SIGNED_AT);
    assert!(matches!(refusal(result), EvalError::Core(_)));
}

#[test]
fn manifest_path_is_under_research_signed() {
    assert_eq!(
        manifest_path("trend-v1"),
        "research/signed/trend-v1.manifest.json"
    );
}

/// Signs a tree whose config text is `text`, and returns the signed deployment hash.
fn signed_hash_of(text: &str) -> nisse_core::Hash32 {
    let tree = Tree::new();
    tree.write(CONFIG_PATH, text);
    tree.write("research/notes/trend-v1.md", NOTE);
    sign_valid(tree.path()).unwrap().deployment_hash
}

#[test]
fn a_changed_strategy_parameter_changes_the_signed_hash() {
    let changed = CONFIG.replace("\"lookback_days\": 252", "\"lookback_days\": 126");
    assert_ne!(changed, CONFIG);
    assert_ne!(signed_hash_of(&changed), signed_hash_of(CONFIG));
}

#[test]
fn a_changed_symbol_changes_the_signed_hash() {
    let changed = CONFIG.replace("\"VNQ\"]", "\"VXX\"]");
    assert_ne!(changed, CONFIG);
    assert_ne!(signed_hash_of(&changed), signed_hash_of(CONFIG));
}

#[test]
fn a_changed_benchmark_changes_the_signed_hash() {
    let changed = CONFIG.replace("\"benchmark\": \"SPY\"", "\"benchmark\": \"QQQ\"");
    assert_ne!(changed, CONFIG);
    assert_ne!(signed_hash_of(&changed), signed_hash_of(CONFIG));
}

#[test]
fn the_signed_hash_ignores_symbol_order() {
    let reordered = CONFIG.replace(
        "[\"DBC\", \"EEM\", \"EFA\", \"GLD\", \"IEF\", \"SPY\", \"TLT\", \"VNQ\"]",
        "[\"VNQ\", \"TLT\", \"SPY\", \"IEF\", \"GLD\", \"EFA\", \"EEM\", \"DBC\"]",
    );
    assert_ne!(reordered, CONFIG);
    assert_eq!(signed_hash_of(&reordered), signed_hash_of(CONFIG));
}

/// The repository config with its name, family and `created` date changed, and its rationale
/// pointing at its own note.
fn other_config(name: &str, family: &str, created: &str) -> String {
    CONFIG
        .replace("\"name\": \"trend-v1\"", &format!("\"name\": \"{name}\""))
        .replace(
            "\"family\": \"trend\"",
            &format!("\"family\": \"{family}\""),
        )
        .replace(
            "\"created\": \"2026-11-27\"",
            &format!("\"created\": \"{created}\""),
        )
        .replace(
            "research/notes/trend-v1.md",
            &format!("research/notes/{name}.md"),
        )
}

#[test]
fn refuses_when_another_config_shares_the_change_budget() {
    let tree = Tree::valid();
    // Same family as trend-v1 and created the same day: two proposals opened together.
    tree.write(
        "research/configs/trend-old.json",
        &other_config("trend-old", "trendb", "2026-11-27"),
    );
    assert_invalid_proposal(sign_valid(tree.path()), "trend-old");
}

#[test]
fn signs_when_no_other_config_shares_the_change_budget() {
    let tree = Tree::valid();
    tree.write(
        "research/configs/trend-old.json",
        &other_config("trend-old", "rates", "2026-06-01"),
    );
    sign_valid(tree.path()).unwrap();
}

#[cfg(unix)]
#[test]
fn refuses_a_symlinked_config() {
    let tree = Tree::new();
    tree.write("research/elsewhere/trend-v1.json", CONFIG);
    tree.write("research/notes/trend-v1.md", NOTE);
    tree.link(CONFIG_PATH, "../elsewhere/trend-v1.json");
    assert_invalid_proposal(sign_valid(tree.path()), "is not a regular file");
}

#[cfg(unix)]
#[test]
fn refuses_a_symlinked_note() {
    let tree = Tree::new();
    tree.write(CONFIG_PATH, CONFIG);
    tree.write("research/elsewhere/note.md", NOTE);
    tree.link("research/notes/trend-v1.md", "../elsewhere/note.md");
    assert_invalid_proposal(sign_valid(tree.path()), "is not a regular file");
}
