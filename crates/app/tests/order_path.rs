//! Guards the rule that an order reaches a broker only from the trader, after the risk gate's check.
//!
//! This is a tripwire for honest mistakes, not a proof. It reads source text and rejects a new
//! production call that sends or replaces an order, or a new network client dependency. A determined
//! change could route around a text search (an alias, a macro, a re-export under another name), so a
//! pass here does not prove the rule holds. Review of the sensitive paths (`crates/risk/`,
//! `crates/alpaca-adapter/`, `config/limits/`, `.github/`) remains the control.
//!
//! The functions that send orders are public (the engine's own integration tests need them), so a
//! new production caller fails this test. If it fails because you added one on purpose, that is a
//! change to a sensitive path: say so in the pull request, then update the lists here.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

/// Crate dependencies that open network connections. A crate other than `alpaca-adapter` that
/// depends on one of these fails the network test.
const NETWORK_CLIENTS: [&str; 8] = [
    "reqwest",
    "tungstenite",
    "tokio-tungstenite",
    "hyper",
    "ureq",
    "isahc",
    "surf",
    "curl",
];

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

/// Whether the attributes and blank lines that follow a `#[cfg(test)]` line open a `mod` item.
/// Attributes may span lines (`#[allow(` ... `)]`), so their brackets are counted.
fn opens_test_module(rest: &[&str]) -> bool {
    let mut in_attr = false;
    let mut depth: i64 = 0;
    for line in rest {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if in_attr || t.starts_with("#[") {
            if !in_attr {
                in_attr = true;
                depth = 0;
            }
            for c in t.chars() {
                match c {
                    '[' => depth += 1,
                    ']' => depth -= 1,
                    _ => {}
                }
            }
            if depth <= 0 {
                in_attr = false;
            }
            continue;
        }
        return t.starts_with("mod ");
    }
    false
}

/// Production text of a file: comment lines are removed, and so is a trailing test module.
///
/// A `#[cfg(test)]` line ends the text (to the end of the file) only when the next line that is not
/// blank and not an attribute starts with `mod `, the workspace's test-module convention. Any other
/// `#[cfg(test)]` item, such as a test helper, stays in the text.
fn production_text(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut kept = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if line.trim() == "#[cfg(test)]"
            && opens_test_module(lines.get(i + 1..).unwrap_or_default())
        {
            break;
        }
        if line.trim_start().starts_with("//") {
            continue;
        }
        kept.push(*line);
    }
    kept.join("\n")
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Byte offsets where `name` occurs as a whole identifier (not inside a longer one).
fn ident_positions<'a>(text: &'a str, name: &'a str) -> impl Iterator<Item = usize> + 'a {
    text.match_indices(name).filter_map(move |(at, _)| {
        let before_ok = text[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !is_ident_char(c));
        let after_ok = text[at + name.len()..]
            .chars()
            .next()
            .is_none_or(|c| !is_ident_char(c));
        (before_ok && after_ok).then_some(at)
    })
}

/// Whether `name` is called as a method or path item: `.name(`, `::name(`, or with a turbofish.
fn calls_method(text: &str, name: &str) -> bool {
    ident_positions(text, name).any(|at| {
        let before = &text[..at];
        let after = &text[at + name.len()..];
        (before.ends_with('.') || before.ends_with("::"))
            && (after.starts_with('(') || after.starts_with("::<"))
    })
}

/// Immediate subdirectories of `parent`, sorted by name.
fn subdirs(parent: &Path) -> Vec<(String, PathBuf)> {
    let mut dirs: Vec<(String, PathBuf)> = fs::read_dir(parent)
        .expect("read parent directory")
        .map(|entry| entry.expect("read directory entry").path())
        .filter(|p| p.is_dir())
        .map(|p| {
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .expect("directory name is UTF-8")
                .to_owned();
            (name, p)
        })
        .collect();
    dirs.sort();
    dirs
}

fn crate_dirs() -> Vec<(String, PathBuf)> {
    subdirs(&root().join("crates"))
}

/// Production source files of the workspace crates and of the phone app's Rust module
/// (`ui/ext/*`, a separate workspace), as (repo-relative path with forward slashes, production text).
fn sources() -> Vec<(String, String)> {
    let root = root();
    let mut crate_roots: Vec<PathBuf> = crate_dirs().into_iter().map(|(_, dir)| dir).collect();
    crate_roots.extend(
        subdirs(&root.join("ui/ext"))
            .into_iter()
            .map(|(_, dir)| dir),
    );
    let mut result = Vec::new();
    for dir in crate_roots {
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

/// Paths of the sources whose production text satisfies `pred`, in sorted order.
fn files_where(sources: &[(String, String)], pred: impl Fn(&str) -> bool) -> Vec<&str> {
    sources
        .iter()
        .filter(|(_, text)| pred(text))
        .map(|(path, _)| path.as_str())
        .collect()
}

/// The dependency name at the start of a manifest line, or "" when the line has none.
fn dependency_name(line: &str) -> &str {
    let end = line
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .unwrap_or(line.len());
    &line[..end]
}

#[test]
fn production_text_keeps_a_cfg_test_helper_and_the_code_after_it() {
    let text = "#[cfg(test)]\nfn helper() {}\n\nfn ship(b: &B) {\n    b.submit(i);\n}\n";
    let kept = production_text(text);
    assert!(
        kept.contains("b.submit(i);"),
        "production call dropped: {kept}"
    );
    assert!(kept.contains("fn helper()"), "helper dropped: {kept}");
}

#[test]
fn production_text_drops_a_trailing_test_module() {
    let text = "fn ship() {}\n#[cfg(test)]\nmod tests {\n    fn t() { x.submit(i); }\n}\n";
    let kept = production_text(text);
    assert!(
        kept.contains("fn ship()"),
        "production code dropped: {kept}"
    );
    assert!(!kept.contains("submit"), "test module kept: {kept}");
}

#[test]
fn production_text_drops_a_test_module_behind_an_attribute() {
    let one_line = "fn ship() {}\n#[cfg(test)]\n#[allow(clippy::unwrap_used)]\nmod tests {\n    fn t() { x.submit(i); }\n}\n";
    assert!(!production_text(one_line).contains("submit"));

    let multi_line = "fn ship() {}\n#[cfg(test)]\n#[allow(\n    clippy::unwrap_used,\n    clippy::panic,\n)]\n\nmod tests {\n    fn t() { x.replace(i); }\n}\n";
    let kept = production_text(multi_line);
    assert!(
        kept.contains("fn ship()"),
        "production code dropped: {kept}"
    );
    assert!(!kept.contains("replace"), "test module kept: {kept}");
}

#[test]
fn production_text_drops_comment_lines_only() {
    let text = "// x.submit(i);\nfn ship() {\n    // y.submit(i);\n    z.submit(i);\n}\n";
    let kept = production_text(text);
    assert!(!kept.contains("// "), "comment kept: {kept}");
    assert!(kept.contains("z.submit(i);"), "call dropped: {kept}");
}

#[test]
fn identifiers_match_whole_words_only() {
    assert!(calls_method("a.submit(x)", "submit"));
    assert!(calls_method("Broker::submit(&b, x)", "submit"));
    assert!(calls_method("g.submit::<T>(x)", "submit"));
    assert!(!calls_method("fn submit(x)", "submit"));
    assert!(!calls_method("a.submit_and_record(x)", "submit"));
    assert!(!calls_method("a.resubmit(x)", "submit"));
    assert!(ident_positions("submit_and_record::<T>(x)", "submit_and_record").count() == 1);
    assert!(ident_positions("resubmit_and_record", "submit_and_record").count() == 0);
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
    assert!(
        sources
            .iter()
            .any(|(p, _)| p.starts_with("ui/ext/nisse-native/src/")),
        "ui/ext/nisse-native/src/ was not found by the source walk"
    );
}

#[test]
fn only_the_trader_submits_orders() {
    let sources = sources();
    assert_eq!(
        files_where(&sources, |t| ident_positions(t, "submit_and_record")
            .next()
            .is_some()),
        [
            "crates/engine/src/lib.rs",
            "crates/engine/src/order_service.rs",
            "crates/engine/src/trader.rs",
        ],
        "submit_and_record may appear only in the re-export, its definition and the trader"
    );
}

#[test]
fn nothing_replaces_orders_outside_tests() {
    let sources = sources();
    assert_eq!(
        files_where(&sources, |t| ident_positions(t, "replace_and_record")
            .next()
            .is_some()),
        [
            "crates/engine/src/lib.rs",
            "crates/engine/src/order_service.rs",
        ],
        "replace_and_record may appear only in the re-export and its definition"
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
    for name in ["submit", "replace"] {
        assert_eq!(
            files_where(&sources, |t| calls_method(t, name)),
            expected,
            "callers of `{name}` changed"
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
    let submit = "submit_and_record";
    assert_eq!(
        trader.matches(check).count(),
        1,
        "expected exactly one `{check}` in trader.rs"
    );
    // The `use` of the function and its one call.
    assert_eq!(
        ident_positions(trader, submit).count(),
        2,
        "expected exactly two `{submit}` in trader.rs (the use and the call)"
    );
    let check_at = trader.find(check).expect("gate check present");
    let submit_at = ident_positions(trader, submit)
        .last()
        .expect("submit present");
    assert!(
        check_at < submit_at,
        "the risk gate check must come before the last `{submit}`"
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
        .filter(|(_, text)| {
            text.lines()
                .any(|line| NETWORK_CLIENTS.contains(&dependency_name(line.trim_start())))
        })
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(
        networked,
        ["alpaca-adapter"],
        "only the broker adapter may depend on a network client"
    );
}
