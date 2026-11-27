//! Acceptance tests for research configs, notes and the proposal check. The checks read only
//! small directory trees that each test builds under a unique temporary directory, removed
//! when the test ends.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use chrono::NaiveDate;
use nisse_eval::proposal::{Change, ResearchConfig, check_note, check_proposal, parse_changes};
use serde_json::{Value, json};

/// "Today" for every test.
fn today() -> NaiveDate {
    date(2026, 11, 27)
}

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

/// A date `days` after 2026-01-01.
fn day(days: i64) -> NaiveDate {
    date(2026, 1, 1) + chrono::TimeDelta::days(days)
}

const VALID_NOTE: &str = "# note\n\n## Economic rationale\n\nA reason that is written here.\n\n\
## Expected effect\n\nAn effect, stated before any result.\n\n\
## What would change my mind\n\nA result that would make me stop.\n";

const EFFECT: &str = "A diversified trend book with a long enough effect text to pass the rule.";

/// A temporary directory tree, removed on drop.
struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new() -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("nisse-proposal-{}-{n}", std::process::id()));
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

    /// Writes a config file `research/configs/<name>.json` and its note.
    fn put(&self, config: &Value) {
        let name = config["name"].as_str().unwrap();
        self.write(
            &format!("research/configs/{name}.json"),
            &config.to_string(),
        );
        self.write(&format!("research/notes/{name}.md"), VALID_NOTE);
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// A valid config as JSON, with the fields the tests vary.
fn config(name: &str, family: &str, created: NaiveDate, parent: Value) -> Value {
    json!({
        "name": name,
        "family": family,
        "created": created.format("%Y-%m-%d").to_string(),
        "parent": parent,
        "strategy": {
            "strategy": "trend_vol_target",
            "lookback_days": 252,
            "skip_days": 21,
            "vol_window": 60,
            "target_vol": "0.09",
            "max_weight": "0.125",
            "min_trade_notional": "25",
            "sessions_per_year": 252
        },
        "symbols": ["SPY", "TLT"],
        "benchmark": "SPY",
        "rationale": format!("research/notes/{name}.md"),
        "expected_effect": EFFECT
    })
}

fn added(path: &str) -> Change {
    Change::Added(path.to_owned())
}

fn modified(path: &str) -> Change {
    Change::Modified(path.to_owned())
}

/// The change list for adding one config and its note.
fn added_config(name: &str) -> Vec<Change> {
    vec![
        added(&format!("research/configs/{name}.json")),
        added(&format!("research/notes/{name}.md")),
    ]
}

/// Runs the check with `base` and `head` as the two trees.
fn run(changes: &[Change], base: &Tree, head: &Tree) -> Vec<String> {
    check_proposal(changes, base.path(), head.path(), today())
}

/// Two trees that start with the same base contents: `base_configs` written to both.
fn trees(base_configs: &[Value]) -> (Tree, Tree) {
    let base = Tree::new();
    let head = Tree::new();
    for c in base_configs {
        base.put(c);
        head.put(c);
    }
    (base, head)
}

// ---- the repository's own files ---------------------------------------------------------------

#[test]
fn the_repository_config_and_note_are_valid() {
    let config = ResearchConfig::parse(include_str!("../../../research/configs/trend-v1.json"))
        .expect("trend-v1 parses");
    assert_eq!(config.name, "trend-v1");
    check_note(include_str!("../../../research/notes/trend-v1.md")).expect("trend-v1 note passes");
}

// ---- ResearchConfig::parse -------------------------------------------------------------------

fn parse_error(value: &Value) -> String {
    ResearchConfig::parse(&value.to_string())
        .expect_err("config should be refused")
        .to_string()
}

#[test]
fn parse_accepts_a_valid_config_and_sorts_symbols() {
    let mut value = config("trend-v1", "trend", day(0), Value::Null);
    value["symbols"] = json!(["TLT", "SPY", "EEM"]);
    let parsed = ResearchConfig::parse(&value.to_string()).unwrap();
    assert_eq!(parsed.symbols, vec!["EEM", "SPY", "TLT"]);
    assert_eq!(parsed.parent, None);
    assert_eq!(parsed.created, day(0));
}

#[test]
fn parse_refuses_an_unknown_field() {
    let mut value = config("trend-v1", "trend", day(0), Value::Null);
    value["extra"] = json!(1);
    assert!(parse_error(&value).contains("extra"));
}

#[test]
fn parse_refuses_a_missing_parent() {
    let mut value = config("trend-v1", "trend", day(0), Value::Null);
    value.as_object_mut().unwrap().remove("parent");
    assert!(parse_error(&value).contains("parent"));
}

#[test]
fn parse_refuses_a_bad_name() {
    for bad in ["Trend", "trend--v1", "-trend", "trend-", "", "trend_v1"] {
        let value = config(bad, "trend", day(0), Value::Null);
        assert!(parse_error(&value).contains("name"), "name {bad:?}");
    }
    let long = "a".repeat(49);
    let value = config(&long, "trend", day(0), Value::Null);
    assert!(parse_error(&value).contains("name"));
}

#[test]
fn parse_refuses_a_name_with_a_double_hyphen_even_if_short() {
    let value = config("a--b", "trend", day(0), Value::Null);
    assert!(parse_error(&value).contains("name"));
}

#[test]
fn parse_refuses_a_bad_family() {
    let value = config("trend-v1", &"f".repeat(33), day(0), Value::Null);
    assert!(parse_error(&value).contains("family"));
    let value = config("trend-v1", "Trend", day(0), Value::Null);
    assert!(parse_error(&value).contains("family"));
}

#[test]
fn parse_refuses_a_bad_date() {
    for bad in [
        "2026-1-05",
        "2026/01/05",
        "2026-02-30",
        "20260105",
        "2026-01-05T",
    ] {
        let mut value = config("trend-v1", "trend", day(0), Value::Null);
        value["created"] = json!(bad);
        assert!(parse_error(&value).contains("created"), "date {bad:?}");
    }
}

#[test]
fn parse_refuses_a_parent_equal_to_the_name() {
    let value = config("trend-v1", "trend", day(0), json!("trend-v1"));
    assert!(parse_error(&value).contains("parent"));
}

#[test]
fn parse_refuses_a_parent_that_is_not_a_name() {
    let value = config("trend-v1", "trend", day(0), json!("Trend_V0"));
    assert!(parse_error(&value).contains("parent"));
    let value = config("trend-v1", "trend", day(0), json!(3));
    assert!(parse_error(&value).contains("parent"));
}

#[test]
fn parse_refuses_an_unknown_strategy() {
    let mut value = config("trend-v1", "trend", day(0), Value::Null);
    value["strategy"] = json!({"strategy": "no_such_strategy"});
    assert!(parse_error(&value).contains("strategy"));
    value["strategy"] = json!("trend_vol_target");
    assert!(parse_error(&value).contains("strategy"));
}

#[test]
fn parse_refuses_empty_symbols() {
    let mut value = config("trend-v1", "trend", day(0), Value::Null);
    value["symbols"] = json!([]);
    assert!(parse_error(&value).contains("symbols"));
}

#[test]
fn parse_refuses_a_duplicate_symbol() {
    let mut value = config("trend-v1", "trend", day(0), Value::Null);
    value["symbols"] = json!(["SPY", "TLT", "SPY"]);
    assert!(parse_error(&value).contains("SPY"));
}

#[test]
fn parse_refuses_a_lower_case_symbol() {
    let mut value = config("trend-v1", "trend", day(0), Value::Null);
    value["symbols"] = json!(["spy"]);
    assert!(parse_error(&value).contains("symbols"));
}

#[test]
fn parse_refuses_a_symbol_that_is_too_long_or_has_bad_characters() {
    let mut value = config("trend-v1", "trend", day(0), Value::Null);
    value["symbols"] = json!(["ABCDEFGHIJKLM"]);
    assert!(parse_error(&value).contains("symbols"));
    value["symbols"] = json!(["SP-Y"]);
    assert!(parse_error(&value).contains("symbols"));
    value["symbols"] = json!(["SPY"]);
    value["benchmark"] = json!("spy");
    assert!(parse_error(&value).contains("benchmark"));
}

#[test]
fn parse_refuses_a_wrong_rationale_path() {
    let mut value = config("trend-v1", "trend", day(0), Value::Null);
    value["rationale"] = json!("research/notes/other.md");
    assert!(parse_error(&value).contains("rationale"));
}

#[test]
fn parse_refuses_a_short_expected_effect() {
    let mut value = config("trend-v1", "trend", day(0), Value::Null);
    value["expected_effect"] = json!("   too short    ");
    assert!(parse_error(&value).contains("expected_effect"));
}

#[test]
fn parse_refuses_invalid_json() {
    assert!(ResearchConfig::parse("{").is_err());
}

// ---- check_note ------------------------------------------------------------------------------

#[test]
fn check_note_accepts_the_three_headings_with_text() {
    check_note(VALID_NOTE).unwrap();
}

#[test]
fn check_note_refuses_a_missing_heading() {
    let text = VALID_NOTE.replace("## Expected effect", "## Effect");
    assert!(check_note(&text).is_err());
}

#[test]
fn check_note_refuses_headings_out_of_order() {
    let text = "## What would change my mind\n\nx\n\n## Expected effect\n\ny\n\n\
                ## Economic rationale\n\nz\n";
    assert!(check_note(text).is_err());
}

#[test]
fn check_note_refuses_a_heading_with_no_text_under_it() {
    let text = VALID_NOTE.replace(
        "## Expected effect\n\nAn effect, stated before any result.\n\n",
        "## Expected effect\n\n",
    );
    assert!(check_note(&text).is_err());
    // Only a nested heading does not count as text.
    let text = VALID_NOTE.replace(
        "## Expected effect\n\nAn effect, stated before any result.\n\n",
        "## Expected effect\n### detail\n\n",
    );
    assert!(check_note(&text).is_err());
}

#[test]
fn check_note_refuses_the_last_heading_with_no_text() {
    let text = VALID_NOTE.replace("A result that would make me stop.\n", "");
    assert!(check_note(&text).is_err());
}

// ---- parse_changes ---------------------------------------------------------------------------

#[test]
fn parse_changes_reads_each_status() {
    let text = "A\ta.json\nM\tb.json\nD\tc.json\nR100\told.md\tnew.md\nT\td\n";
    let changes = parse_changes(text).unwrap();
    assert_eq!(
        changes,
        vec![
            Change::Added("a.json".into()),
            Change::Modified("b.json".into()),
            Change::Deleted("c.json".into()),
            Change::Renamed {
                from: "old.md".into(),
                to: "new.md".into()
            },
            Change::Other {
                status: "T".into(),
                path: "d".into()
            },
        ]
    );
}

#[test]
fn parse_changes_skips_blank_lines() {
    let changes = parse_changes("\nA\ta.json\n\n   \nM\tb.json\n").unwrap();
    assert_eq!(changes.len(), 2);
    assert!(parse_changes("").unwrap().is_empty());
}

#[test]
fn parse_changes_refuses_a_line_with_no_tab() {
    assert!(parse_changes("M README.md\n").is_err());
    assert!(parse_changes("A\ta.json\nnonsense\n").is_err());
}

#[test]
fn parse_changes_refuses_the_wrong_number_of_paths() {
    assert!(parse_changes("R100\tonly-one\n").is_err());
    assert!(parse_changes("M\ta\tb\n").is_err());
}

// ---- check_proposal --------------------------------------------------------------------------

#[test]
fn a_change_outside_research_passes_untouched() {
    let (base, head) = trees(&[]);
    let changes = vec![modified("crates/x.rs")];
    assert!(run(&changes, &base, &head).is_empty());
}

#[test]
fn adding_the_first_config_of_a_new_family_passes() {
    let (base, head) = trees(&[]);
    head.put(&config("trend-v1", "trend", today(), Value::Null));
    let changes = added_config("trend-v1");
    assert_eq!(run(&changes, &base, &head), Vec::<String>::new());
}

#[test]
fn outside_paths_in_a_research_pr_fail_and_are_named() {
    let (base, head) = trees(&[]);
    head.put(&config("trend-v1", "trend", today(), Value::Null));
    for outside in [
        "crates/risk/src/gate.rs",
        ".github/workflows/ci.yml",
        "config/limits/limits.json",
        "research/signed/x.json",
        "research/agents/overnight.md",
        "research/README.md",
    ] {
        let mut changes = added_config("trend-v1");
        changes.push(modified(outside));
        let failures = run(&changes, &base, &head);
        assert!(
            failures.iter().any(|f| f.contains(outside)),
            "{outside}: {failures:?}"
        );
    }
}

#[test]
fn deleting_a_note_fails() {
    let (base, head) = trees(&[config("trend-v1", "trend", day(0), Value::Null)]);
    let changes = vec![Change::Deleted("research/notes/trend-v1.md".into())];
    assert_eq!(run(&changes, &base, &head).len(), 1);
}

#[test]
fn renaming_a_note_fails() {
    let (base, head) = trees(&[config("trend-v1", "trend", day(0), Value::Null)]);
    let changes = vec![Change::Renamed {
        from: "research/notes/trend-v1.md".into(),
        to: "research/notes/trend-v2.md".into(),
    }];
    assert!(!run(&changes, &base, &head).is_empty());
}

#[test]
fn two_configs_added_fail() {
    let (base, head) = trees(&[]);
    head.put(&config("trend-a", "trend", day(0), Value::Null));
    head.put(&config("trend-b", "trend", day(0), Value::Null));
    let mut changes = added_config("trend-a");
    changes.extend(added_config("trend-b"));
    assert!(!run(&changes, &base, &head).is_empty());
}

#[test]
fn a_config_in_a_subdirectory_or_not_json_fails() {
    let (base, head) = trees(&[]);
    head.put(&config("trend-v1", "trend", today(), Value::Null));
    let changes = vec![added("research/configs/sub/trend-v1.json")];
    assert!(!run(&changes, &base, &head).is_empty());
    let changes = vec![added("research/configs/trend-v1.txt")];
    assert!(!run(&changes, &base, &head).is_empty());
}

#[test]
fn a_config_without_its_note_fails() {
    let (base, head) = trees(&[]);
    head.write(
        "research/configs/trend-v1.json",
        &config("trend-v1", "trend", today(), Value::Null).to_string(),
    );
    let changes = vec![added("research/configs/trend-v1.json")];
    let failures = run(&changes, &base, &head);
    assert!(failures.iter().any(|f| f.contains("note")), "{failures:?}");
}

#[test]
fn a_note_without_a_heading_fails() {
    let (base, head) = trees(&[]);
    head.put(&config("trend-v1", "trend", today(), Value::Null));
    head.write("research/notes/trend-v1.md", "## Economic rationale\n\nx\n");
    let failures = run(&added_config("trend-v1"), &base, &head);
    assert!(!failures.is_empty());
}

#[test]
fn a_file_stem_that_differs_from_the_name_fails() {
    let (base, head) = trees(&[]);
    let value = config("trend-b", "trend", today(), Value::Null);
    head.write("research/configs/other.json", &value.to_string());
    head.write("research/notes/trend-b.md", VALID_NOTE);
    let changes = vec![added("research/configs/other.json")];
    let failures = run(&changes, &base, &head);
    assert!(
        failures.iter().any(|f| f.contains("other.json")),
        "{failures:?}"
    );
}

#[test]
fn a_config_created_in_the_future_fails() {
    let (base, head) = trees(&[]);
    head.put(&config(
        "trend-v1",
        "trend",
        date(2026, 11, 28),
        Value::Null,
    ));
    assert!(!run(&added_config("trend-v1"), &base, &head).is_empty());
}

#[test]
fn a_second_config_of_a_family_29_days_after_the_first_fails() {
    let first = config("trend-a", "trend", day(0), Value::Null);
    let (base, head) = trees(std::slice::from_ref(&first));
    head.put(&config("trend-b", "trend", day(29), json!("trend-a")));
    assert!(!run(&added_config("trend-b"), &base, &head).is_empty());
}

#[test]
fn a_second_config_of_a_family_30_days_after_the_first_passes() {
    let first = config("trend-a", "trend", day(0), Value::Null);
    let (base, head) = trees(std::slice::from_ref(&first));
    head.put(&config("trend-b", "trend", day(30), json!("trend-a")));
    assert_eq!(
        run(&added_config("trend-b"), &base, &head),
        Vec::<String>::new()
    );
}

#[test]
fn a_parent_missing_at_the_base_fails() {
    let (base, head) = trees(&[]);
    head.put(&config("trend-b", "trend", day(40), json!("trend-a")));
    assert!(!run(&added_config("trend-b"), &base, &head).is_empty());
}

#[test]
fn a_parent_of_another_family_fails() {
    let other = config("rates-a", "rates", day(0), Value::Null);
    let (base, head) = trees(std::slice::from_ref(&other));
    head.put(&config("trend-b", "trend", day(40), json!("rates-a")));
    let failures = run(&added_config("trend-b"), &base, &head);
    assert!(
        failures.iter().any(|f| f.contains("family")),
        "{failures:?}"
    );
}

#[test]
fn null_parent_when_the_family_exists_fails() {
    let first = config("trend-a", "trend", day(0), Value::Null);
    let (base, head) = trees(std::slice::from_ref(&first));
    head.put(&config("trend-b", "trend", day(40), Value::Null));
    let failures = run(&added_config("trend-b"), &base, &head);
    assert!(
        failures.iter().any(|f| f.contains("parent")),
        "{failures:?}"
    );
}

#[test]
fn modifying_a_config_in_place_after_30_days_passes() {
    let old = config("trend-v1", "trend", day(0), Value::Null);
    let (base, head) = trees(std::slice::from_ref(&old));
    // Head: same name and family, a newer date, a different parameter.
    let mut new = config("trend-v1", "trend", day(31), Value::Null);
    new["strategy"]["lookback_days"] = json!(126);
    head.put(&new);
    let changes = vec![modified("research/configs/trend-v1.json")];
    assert_eq!(run(&changes, &base, &head), Vec::<String>::new());
}

#[test]
fn modifying_a_config_in_place_too_soon_fails() {
    let old = config("trend-v1", "trend", day(0), Value::Null);
    let (base, head) = trees(std::slice::from_ref(&old));
    head.put(&config("trend-v1", "trend", day(10), Value::Null));
    let changes = vec![modified("research/configs/trend-v1.json")];
    assert!(!run(&changes, &base, &head).is_empty());
}

#[test]
fn modifying_a_config_into_another_family_fails() {
    let old = config("trend-v1", "trend", day(0), Value::Null);
    let (base, head) = trees(std::slice::from_ref(&old));
    head.put(&config("trend-v1", "rates", day(40), Value::Null));
    let changes = vec![modified("research/configs/trend-v1.json")];
    let failures = run(&changes, &base, &head);
    assert!(
        failures.iter().any(|f| f.contains("family")),
        "{failures:?}"
    );
}

#[test]
fn a_path_with_dot_dot_fails_and_reads_nothing_outside() {
    let (base, head) = trees(&[]);
    let changes = vec![added("research/configs/../../x")];
    let failures = run(&changes, &base, &head);
    assert!(!failures.is_empty());
    assert!(
        failures
            .iter()
            .any(|f| f.contains("research/configs/../../x"))
    );
}

#[test]
fn several_violations_are_all_reported() {
    let (base, head) = trees(&[]);
    head.put(&config("trend-a", "trend", date(2026, 12, 1), Value::Null));
    head.put(&config("trend-b", "trend", date(2026, 12, 1), Value::Null));
    let mut changes = added_config("trend-a");
    changes.extend(added_config("trend-b"));
    changes.push(modified("crates/risk/src/gate.rs"));
    let failures = run(&changes, &base, &head);
    assert!(failures.len() >= 2, "{failures:?}");
}
