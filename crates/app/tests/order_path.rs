//! Guards the rule that an order reaches a broker only from the trader, after the risk gate's check.
//!
//! The functions that send orders are public (the engine's own integration tests need them), so
//! this test reads the source and fails when a new non-test caller appears. If it fails because you
//! added one on purpose, that is a change to a sensitive path: say so in the pull request, then
//! update the list here.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).expect("read source directory");
    for entry in entries {
        let path = entry.expect("read directory entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Production text of a file: lines before the first `#[cfg(test)]`, without comment lines.
fn production_text(text: &str) -> String {
    let mut lines = Vec::new();
    for line in text.lines() {
        if line.trim() == "#[cfg(test)]" {
            break;
        }
        if line.trim_start().starts_with("//") {
            continue;
        }
        lines.push(line);
    }
    lines.join("\n")
}

fn crate_dirs() -> Vec<(String, PathBuf)> {
    let crates = root().join("crates");
    let mut dirs: Vec<(String, PathBuf)> = fs::read_dir(&crates)
        .expect("read crates directory")
        .map(|entry| entry.expect("read directory entry").path())
        .filter(|p| p.is_dir())
        .map(|p| {
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .expect("crate directory name is UTF-8")
                .to_owned();
            (name, p)
        })
        .collect();
    dirs.sort();
    dirs
}

fn sources() -> Vec<(String, String)> {
    let root = root();
    let mut result = Vec::new();
    for (_name, dir) in crate_dirs() {
        let src = dir.join("src");
        if !src.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        rust_files(&src, &mut files);
        for file in files {
            let rel = file
                .strip_prefix(&root)
                .expect("source file lies under the workspace root")
                .to_string_lossy()
                .replace('\\', "/");
            let text = fs::read_to_string(&file).expect("read source file");
            result.push((rel, production_text(&text)));
        }
    }
    result.sort_by(|a, b| a.0.cmp(&b.0));
    result
}

fn files_containing<'a>(sources: &'a [(String, String)], needle: &str) -> Vec<&'a str> {
    sources
        .iter()
        .filter(|(_, text)| text.contains(needle))
        .map(|(path, _)| path.as_str())
        .collect()
}

#[test]
fn sources_are_found() {
    let sources = sources();
    assert!(
        sources.len() > 40,
        "expected more than 40 source files, found {}",
        sources.len()
    );
    assert!(
        sources
            .iter()
            .any(|(p, _)| p == "crates/engine/src/trader.rs"),
        "crates/engine/src/trader.rs was not found by the source walk"
    );
}

#[test]
fn only_the_trader_submits_orders() {
    let sources = sources();
    assert_eq!(
        files_containing(&sources, "submit_and_record("),
        ["crates/engine/src/trader.rs"],
        "submit_and_record( must be called only from the trader"
    );
}

#[test]
fn nothing_replaces_orders_outside_tests() {
    let sources = sources();
    let found = files_containing(&sources, "replace_and_record(");
    assert!(
        found.is_empty(),
        "replace_and_record( must not be called outside tests, found in {found:?}"
    );
}

#[test]
fn only_the_gateway_layer_calls_the_broker_to_submit_or_replace() {
    let sources = sources();
    let expected = [
        "crates/core/src/order.rs",
        "crates/engine/src/gateway.rs",
        "crates/engine/src/order_service.rs",
    ];
    for needle in [".submit(", ".replace("] {
        assert_eq!(
            files_containing(&sources, needle),
            expected,
            "callers of `{needle}` changed"
        );
    }
}

#[test]
fn the_trader_checks_the_risk_gate_before_it_submits() {
    let sources = sources();
    let (_, trader) = sources
        .iter()
        .find(|(p, _)| p == "crates/engine/src/trader.rs")
        .expect("trader.rs is present");

    let check = "self.gate.check(";
    let submit = "submit_and_record(";
    assert_eq!(
        trader.matches(check).count(),
        1,
        "expected exactly one `{check}` in trader.rs"
    );
    assert_eq!(
        trader.matches(submit).count(),
        1,
        "expected exactly one `{submit}` in trader.rs"
    );
    let check_at = trader.find(check).expect("gate check present");
    let submit_at = trader.find(submit).expect("submit present");
    assert!(
        check_at < submit_at,
        "the risk gate check must come before submit_and_record("
    );
}

#[test]
fn only_the_broker_adapter_can_reach_the_network() {
    let mut manifests: Vec<(String, String)> = Vec::new();
    for (name, dir) in crate_dirs() {
        let text = fs::read_to_string(dir.join("Cargo.toml")).expect("read crate Cargo.toml");
        manifests.push((name, text));
    }
    manifests.sort_by(|a, b| a.0.cmp(&b.0));
    let networked: Vec<&str> = manifests
        .iter()
        .filter(|(_, text)| text.contains("reqwest") || text.contains("tungstenite"))
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(
        networked,
        ["alpaca-adapter"],
        "only the broker adapter may depend on a network client"
    );
}
